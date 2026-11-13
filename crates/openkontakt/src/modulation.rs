//! Named modulation records and external controller curves (format notes in docs/format.md).
//! Unknown records are diagnosed and retained in the original Group, never given invented routes.
use crate::Ahdsr;
use crate::group::Group;

pub const EXTERNAL_HANDLE: i64 = 0x10000;

#[derive(Clone, Debug)]
pub struct CurvePoint {
    pub x: f32,
    pub y: f32,
    pub shape: f32,
}
#[derive(Clone, Debug)]
pub struct Curve {
    pub bipolar: bool,
    pub points: Vec<CurvePoint>,
}
impl Curve {
    pub fn value(&self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        let mut y = self.points.last().map(|p| p.y).unwrap_or(x);
        for pair in self.points.windows(2) {
            if x <= pair[1].x {
                let t = ((x - pair[0].x) / (pair[1].x - pair[0].x).max(1e-7)).clamp(0.0, 1.0);
                // Endpoints come from the file; the precise native segment curvature is open.
                let exponent = 2f32.powf(-2.0 * pair[0].shape.clamp(-1.0, 1.0));
                y = pair[0].y + (pair[1].y - pair[0].y) * t.powf(exponent);
                break;
            }
        }
        if self.bipolar { 2.0 * y - 1.0 } else { y }
    }
}

#[derive(Clone, Debug)]
pub struct Target {
    pub parameter: String,
    pub name: String,
    pub destination: Option<u8>,
    pub amount: f32,
    pub inverted: bool,
    pub curve: Option<Curve>,
}
impl Target {
    pub fn value(&self, input: f32) -> f32 {
        let input = if self.inverted { 1.0 - input } else { input };
        self.curve.as_ref().map(|c| c.value(input)).unwrap_or(input)
    }
}

#[derive(Clone, Debug)]
pub enum Source {
    Internal,
    Controller { number: u8, raw_assignment: [u8; 4] },
    Velocity,
    Other(u32),
}
#[derive(Clone, Debug)]
pub struct Modulator {
    pub handle: i64,
    pub name: String,
    pub targets: Vec<Target>,
    pub source: Source,
    pub envelope: Option<Ahdsr>,
}
#[derive(Clone, Debug, Default)]
pub struct GroupModulation {
    pub modulators: Vec<Modulator>,
    pub diagnostics: Vec<String>,
}
impl GroupModulation {
    pub fn find(&self, name: &str) -> Option<&Modulator> {
        self.modulators.iter().find(|m| m.name == name)
    }
    pub fn by_handle(&self, handle: i64) -> Option<&Modulator> {
        self.modulators.iter().find(|m| m.handle == handle)
    }
    pub fn amp_slot(&self) -> Option<u8> {
        self.modulators
            .iter()
            .find(|m| m.envelope.is_some() && m.targets.iter().any(|t| t.parameter == "volume"))
            .and_then(|m| u8::try_from(m.handle).ok())
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).ok_or("modulation size overflow")?;
        let out = self
            .data
            .get(self.at..end)
            .ok_or("truncated modulation record")?;
        self.at = end;
        Ok(out)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, String> {
        let v = f32::from_bits(self.u32()?);
        if v.is_finite() {
            Ok(v)
        } else {
            Err("nonfinite modulation value".into())
        }
    }
    fn text(&mut self) -> Result<String, String> {
        let len = self.u32()? as usize;
        if len > 4096 {
            return Err("modulation name too long".into());
        }
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| "invalid modulation name".into())
    }
}

fn target(c: &mut Cursor<'_>, external: bool) -> Result<Target, String> {
    let parameter = c.text()?;
    let amount = c.f32()?;
    c.take(5)?;
    let name = c.text()?;
    let destination = if parameter.starts_with("ahdsr_")
        || parameter.starts_with("arp32step_")
        || matches!(
            parameter.as_str(),
            "filterCutoff" | "formantTalk" | "intensity"
        ) {
        Some(c.u8()?)
    } else if matches!(parameter.as_str(), "volume" | "pan" | "pitch") {
        None
    } else {
        return Err(format!("unestablished modulation target {parameter}"));
    };
    let inverted = match c.u8()? {
        0 => false,
        1 => true,
        _ => return Err("unknown modulation inversion flag".into()),
    };
    let curve = if !external {
        None
    } else {
        match c.u8()? {
            0 => None,
            2 => {
                let bipolar = match c.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err("unknown curve polarity".into()),
                };
                let count = c.u32()? as usize;
                if !(1..=256).contains(&count) {
                    return Err("invalid modulation point count".into());
                }
                if c.u8()? != 0 {
                    return Err("unestablished curve flags".into());
                }
                let mut points = Vec::with_capacity(count);
                points.push(CurvePoint {
                    x: 0.0,
                    y: c.f32()?,
                    shape: c.f32()?,
                });
                for _ in 1..count {
                    let point = CurvePoint {
                        x: c.f32()?,
                        y: c.f32()?,
                        shape: c.f32()?,
                    };
                    if point.x <= points.last().unwrap().x || point.x > 1.0 {
                        return Err("unordered modulation points".into());
                    }
                    points.push(point);
                }
                Some(Curve { bipolar, points })
            }
            mode => return Err(format!("unestablished modulation curve mode {mode}")),
        }
    };
    Ok(Target {
        parameter,
        name,
        destination,
        amount,
        inverted,
        curve,
    })
}

fn parse_modulator(body: &[u8], external: bool, slot: usize) -> Result<Modulator, String> {
    let mut object = Cursor { data: body, at: 0 };
    if object.u8()? != 1 {
        return Err("unstructured modulator".into());
    }
    let version = object.u16()?;
    if version != if external { 0x100 } else { 0x80 } {
        return Err(format!("unestablished modulator version {version:#x}"));
    }
    let len = object.u32()? as usize;
    let public = object.take(len)?;
    let mut c = Cursor {
        data: public,
        at: 0,
    };
    let count = c.u32()? as usize;
    if count > 32 {
        return Err("invalid modulation target count".into());
    }
    let mut targets = Vec::new();
    for _ in 0..count {
        targets.push(target(&mut c, external)?)
    }
    let (name, source) = if external {
        let name = c.text()?;
        if c.u32()? != 1 {
            return Err("multiple modulation sources are not implemented".into());
        }
        let kind = c.u32()?;
        let assignment = c.take(if kind == 4 { 5 } else { 4 })?;
        c.u32()?;
        if c.at != public.len() {
            return Err("unconsumed external modulation fields".into());
        }
        let source = match kind {
            4 => Source::Controller {
                number: assignment[0],
                raw_assignment: assignment[1..5].try_into().unwrap(),
            },
            6 => Source::Velocity,
            other => Source::Other(other),
        };
        (name, source)
    } else {
        // The remaining internal header is opaque. Its final counted name ends exactly before
        // the source-type word, which prevents a scan from confusing target aliases with names.
        let mut name = None;
        for at in c.at..public.len().saturating_sub(7) {
            let mut tail = Cursor { data: public, at };
            if let Ok(value) = tail.text() {
                if tail.at + 4 == public.len()
                    && !value.is_empty()
                    && value.bytes().all(|b| b.is_ascii_graphic())
                {
                    name = Some(value);
                    break;
                }
            }
        }
        (
            name.ok_or("missing internal modulation name")?,
            Source::Internal,
        )
    };
    Ok(Modulator {
        handle: if external {
            EXTERNAL_HANDLE + slot as i64
        } else {
            slot as i64
        },
        name,
        targets,
        source,
        envelope: if external {
            None
        } else {
            crate::envelope::scan_ahdsr(body, 0)
        },
    })
}

pub fn group_modulation(group: &Group) -> GroupModulation {
    let mut out = GroupModulation::default();
    for (id, external) in [(0x3B, false), (0x3C, true)] {
        let Some(array) = group.children.iter().find(|c| c.id == id) else {
            continue;
        };
        if !array.body.starts_with(&[0, 0x12, 0]) {
            out.diagnostics.push(format!(
                "group {}: unestablished modulation array",
                group.index
            ));
            continue;
        }
        let mut c = Cursor {
            data: &array.body,
            at: 3,
        };
        let mut slot = 0;
        while c.at < c.data.len() && slot < 128 {
            let result = (|| -> Result<(), String> {
                match c.u8()? {
                    0 => {}
                    1 => {
                        let class = c.u16()?;
                        let len = c.u32()? as usize;
                        let body = c.take(len)?;
                        if class != if external { 0x0C } else { 0x0D } {
                            return Err("unexpected modulation class".into());
                        }
                        match parse_modulator(body, external, slot) {
                            Ok(m) => out.modulators.push(m),
                            Err(error) => out.diagnostics.push(format!(
                                "group {} modulation {id:#x}/{slot}: {error}",
                                group.index
                            )),
                        }
                    }
                    _ => return Err("invalid modulation presence flag".into()),
                }
                Ok(())
            })();
            if let Err(error) = result {
                out.diagnostics
                    .push(format!("group {}: {error}", group.index));
                break;
            }
            slot += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn text(out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    }
    fn record(external: bool) -> Vec<u8> {
        let mut public = 1u32.to_le_bytes().to_vec();
        text(
            &mut public,
            if external { "ahdsr_attack" } else { "volume" },
        );
        public.extend_from_slice(&0.5f32.to_le_bytes());
        public.extend_from_slice(&[0xff, 0xff, 0x10, 0, 0]);
        text(&mut public, "TARGET");
        if external {
            public.push(0);
        }
        public.push(0);
        if external {
            public.extend_from_slice(&[2, 1]);
            public.extend_from_slice(&2u32.to_le_bytes());
            public.push(0);
            for value in [0.0f32, 0.0, 1.0, 1.0, 0.0] {
                public.extend_from_slice(&value.to_le_bytes());
            }
        } else {
            public.extend_from_slice(&[0; 9]);
        }
        text(&mut public, "SOURCE");
        if external {
            public.extend_from_slice(&1u32.to_le_bytes());
            public.extend_from_slice(&4u32.to_le_bytes());
            public.extend_from_slice(&[84, 0x7f, 0, 0, 0]);
            public.extend_from_slice(&321u32.to_le_bytes());
        } else {
            public.extend_from_slice(&2u32.to_le_bytes());
        }
        let mut body = vec![1];
        body.extend_from_slice(&(if external { 0x100u16 } else { 0x80u16 }).to_le_bytes());
        body.extend_from_slice(&(public.len() as u32).to_le_bytes());
        body.extend_from_slice(&public);
        body.extend_from_slice(&[0; 8]);
        body
    }
    #[test]
    fn parses_addressed_controller_curve_and_keeps_assignment_flags() {
        let m = parse_modulator(&record(true), true, 3).unwrap();
        assert_eq!(m.handle, EXTERNAL_HANDLE + 3);
        assert_eq!(m.name, "SOURCE");
        assert!(matches!(
            m.source,
            Source::Controller {
                number: 84,
                raw_assignment: [0x7f, 0, 0, 0]
            }
        ));
        assert_eq!(m.targets[0].destination, Some(0));
        assert_eq!(m.targets[0].name, "TARGET");
        assert_eq!(m.targets[0].value(0.0), -1.0);
        assert_eq!(m.targets[0].value(1.0), 1.0);
        assert!(m.targets[0].value(0.5).abs() < 1e-6);
    }
    #[test]
    fn internal_lookup_name_is_not_the_target_alias() {
        let m = parse_modulator(&record(false), false, 2).unwrap();
        assert_eq!(m.name, "SOURCE");
        assert_eq!(m.targets[0].name, "TARGET");
        assert_eq!(m.handle, 2);
    }
    #[test]
    fn truncated_records_do_not_invent_routes() {
        let bytes = record(true);
        for length in 0..bytes.len() - 8 {
            assert!(
                parse_modulator(&bytes[..length], true, 0).is_err(),
                "length {length}"
            );
        }
    }
}
