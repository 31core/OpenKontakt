//! Validate every referenced sample and export representative NCWs for independent decoding.
use openkontakt::{Instrument, SampleCursor, load_samples, ncw::NcwFile};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::Arc,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let instrument = Instrument::load(args.next().expect("instrument path")).unwrap();
    let out = PathBuf::from(args.next().expect("output directory"));
    let manifest_only = args.next().as_deref() == Some("--manifest-only");
    fs::create_dir_all(&out).unwrap();
    let mut groups = fs::File::create(out.join("groups.tsv")).unwrap();
    writeln!(groups, "group\tname\trelease_trigger\tkey_tracking").unwrap();
    for g in &instrument.groups {
        writeln!(
            groups,
            "{}\t{}\t{}\t{}",
            g.index, g.name, g.release_trigger, g.key_tracking
        )
        .unwrap();
    }
    let loaded = load_samples(&instrument);
    println!(
        "groups={} zones={} sample_ids={} loose={} packed={} unresolved={} loaded={}",
        instrument.groups.len(),
        instrument.zones.len(),
        instrument.zone_sample_ids().len(),
        instrument.resolved_samples.len(),
        instrument.packed_samples.len(),
        instrument.sample_resolution.len(),
        loaded.samples.len()
    );
    for d in instrument.diagnostics.iter().chain(&loaded.diagnostics) {
        println!("diagnostic {}: {}", d.area, d.message);
    }
    let mut picks = BTreeSet::new();
    for key in [21, 69, 93, 105] {
        for vel in [49, 127] {
            for zone in &instrument.zones {
                if [4, 7, 84, 87].contains(&zone.group_index) && zone.matches(key, vel) {
                    if let Some(s) = &zone.sample {
                        picks.insert(s.filename_id);
                    }
                }
            }
        }
    }
    let mut zones = fs::File::create(out.join("zones.tsv")).unwrap();
    writeln!(zones,"zone\tgroup\tkey_low\tkey_high\tvelocity_low\tvelocity_high\tfilename_id\trequested\tloaded_path\tloaded\tstart\tend\troot").unwrap();
    let mut missing_zones = 0;
    for z in &instrument.zones {
        if let Some(s) = &z.sample {
            let sample = loaded.samples.get(&s.filename_id);
            let requested = instrument
                .filename_table
                .as_ref()
                .and_then(|t| t.sample(s.filename_id))
                .map(|f| f.display_path())
                .unwrap_or_default();
            if sample.is_none() {
                missing_zones += 1;
            }
            writeln!(
                zones,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                z.index,
                z.group_index,
                z.low_key,
                z.high_key,
                z.low_velocity,
                z.high_velocity,
                s.filename_id,
                requested,
                sample
                    .map(|s| s.path.display().to_string())
                    .unwrap_or_default(),
                sample.is_some(),
                z.sample_start,
                z.sample_end,
                z.root_key
            )
            .unwrap();
        }
    }
    if manifest_only {
        return;
    }
    let mut manifest = fs::File::create(out.join("samples.tsv")).unwrap();
    writeln!(
        manifest,
        "id\tpath\tchannels\tbits\trate\tframes\tblocks\tpeak\tstream_mismatches\texported"
    )
    .unwrap();
    let (mut decoded, mut failed, mut mismatches, mut nonfinite) = (0usize, 0usize, 0usize, 0usize);
    for id in instrument.zone_sample_ids() {
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let Some(sample) = loaded.samples.get(&id) else {
                return Err("sample was not loaded".into());
            };
            let bytes = if let Some(path) = instrument.resolved_samples.get(&id) {
                fs::read(path)?
            } else if let Some((path, start, size)) = instrument.packed_sample_record(id) {
                let mut f = fs::File::open(path)?;
                f.seek(SeekFrom::Start(start as u64))?;
                let mut bytes = vec![0; size];
                f.read_exact(&mut bytes)?;
                bytes
            } else {
                return Err("no resolved payload".into());
            };
            if !bytes.starts_with(&openkontakt::ncw::FILE_MAGIC) {
                return Err("audit expects an NCW payload".into());
            }
            let file = NcwFile::parse(&bytes)?;
            let info = file.info();
            let audio = file.decode_all(&bytes)?;
            if audio.samples.len() != info.frames * info.channels as usize {
                return Err("decoded frame count mismatch".into());
            }
            let nf = audio.samples.iter().filter(|v| !v.is_finite()).count();
            nonfinite += nf;
            let peak = audio.samples.iter().fold(0.0f32, |p, v| p.max(v.abs()));
            let mut cursor = SampleCursor::new(Arc::clone(sample));
            let mut bad = 0;
            // Exercise each block boundary in both directions, plus an interior and the final frame.
            for block in 0..file.blocks() {
                for frame in [
                    block * 512,
                    block * 512 + 1,
                    block * 512 + 255,
                    block * 512 + 511,
                ] {
                    if frame >= info.frames {
                        continue;
                    }
                    let (l, r) = cursor.frame(frame);
                    let a = audio.samples[frame * info.channels as usize];
                    let b = if info.channels == 2 {
                        audio.samples[frame * 2 + 1]
                    } else {
                        a
                    };
                    if l.to_bits() != a.to_bits() || r.to_bits() != b.to_bits() {
                        bad += 1;
                    }
                }
            }
            if info.frames > 0 {
                let _ = cursor.frame(info.frames - 1);
            }
            mismatches += bad;
            let export = picks.contains(&id);
            if export {
                fs::write(out.join(format!("sample-{id}.ncw")), &bytes)?;
                let spec = hound::WavSpec {
                    channels: info.channels,
                    sample_rate: info.sample_rate,
                    bits_per_sample: 32,
                    sample_format: hound::SampleFormat::Float,
                };
                let mut writer =
                    hound::WavWriter::create(out.join(format!("sample-{id}-ours.wav")), spec)?;
                for v in audio.samples {
                    writer.write_sample(v)?;
                }
                writer.finalize()?;
            }
            writeln!(
                manifest,
                "{id}\t{}\t{}\t{}\t{}\t{}\t{}\t{peak}\t{bad}\t{export}",
                sample.path.display(),
                info.channels,
                info.bits,
                info.sample_rate,
                info.frames,
                file.blocks()
            )?;
            decoded += 1;
            Ok(())
        })();
        if let Err(e) = result {
            failed += 1;
            println!("FAIL sample {id}: {e}");
        }
        if (decoded + failed) % 100 == 0 {
            println!("checked {} samples", decoded + failed);
        }
    }
    println!(
        "RESULT decoded={decoded} failed={failed} missing_zones={missing_zones} stream_mismatches={mismatches} nonfinite={nonfinite} representative_exports={}",
        picks.len()
    );
    if failed + missing_zones + mismatches + nonfinite > 0 {
        std::process::exit(1);
    }
}
