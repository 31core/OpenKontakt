//! KSP source parser: lexer, AST and a recursive-descent parser.
//!
//! The supported dialect is the subset used by real library panels: `on <callback>` blocks,
//! `declare` (including `ui_*`), assignment, `if`/`else`, `while`, `select`/`case`, `function`,
//! and expressions over `$`/`@`/`!` variables, `%` arrays and constants.
//!
//! KSP is line-oriented: a newline ends a statement, so the parser keeps newlines as tokens. That
//! is what lets `if $x > 0` work without parentheses while still allowing nested expressions.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarKind {
    Int,
    Real,
    Str,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Int(i64),
    Real(f64),
    Str(String),
    /// `$name`, `@name`, `!name`, `%name` — resolved at run time so undeclared names read as zero.
    Var(VarKind, String),
    /// `name[index]`
    Index(Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Concat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiKind {
    Knob,
    Slider,
    Label,
    Menu,
    Switch,
    Button,
    ValueEdit,
    Table,
}

#[derive(Clone, Debug)]
pub struct UiDecl {
    pub kind: UiKind,
    pub name: String,
    /// `ui_table %steps[16](…)` declares a 16-element control. The length is an expression, like
    /// the arguments: `declare ui_table %Sequencer[$max_steps](…)`.
    pub len: Option<Expr>,
    /// Arguments as written. They are evaluated when the declaration runs, not when it is parsed:
    /// retail scripts size a control with expressions like `($Lower_Limit - $KS_Max)`.
    pub args: Vec<Expr>,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Declare {
        kind: VarKind,
        name: String,
        polyphonic: bool,
        init: Option<Expr>,
        /// `declare %interval[20] := (0, 0, 12, 19, …)` — a whole array in one statement.
        array_init: Option<Vec<Expr>>,
        len: Option<Expr>,
    },
    DeclareUi(UiDecl),
    Assign(Expr, Expr),
    Expr(Expr),
    If {
        arms: Vec<(Expr, Vec<Stmt>)>,
        otherwise: Option<Vec<Stmt>>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    Select {
        subject: Expr,
        cases: Vec<(Expr, Vec<Stmt>)>,
        otherwise: Option<Vec<Stmt>>,
    },
    Exit,
}

#[derive(Clone, Debug)]
pub struct Callback {
    pub kind: String,
    pub arg: Option<String>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug, Default)]
pub struct Program {
    pub callbacks: Vec<Callback>,
    pub functions: HashMap<String, Function>,
}

#[derive(Clone, Debug)]
pub struct Function {
    pub params: Vec<(VarKind, String)>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Int(i64),
    Real(f64),
    Str(String),
    Var(VarKind, String),
    Punct(&'static str),
    Newline,
    Eof,
}

fn lex(source: &str) -> Vec<Tok> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b' ' | b'\t' | b'\r' => i += 1,
            b'\n' => {
                out.push(Tok::Newline);
                i += 1;
            }
            // `...` is not KSP, but retail libraries use it as a "values continue here" filler in
            // long array literals, and Kontakt tolerates it. Treat it as a comment to end of line
            // so one such table does not take the whole script slot down with it.
            b'.' if bytes.get(i + 1) == Some(&b'.') && bytes.get(i + 2) == Some(&b'.') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'{' => {
                // `{ ... }` comment, may span lines.
                while i < bytes.len() && bytes[i] != b'}' {
                    if bytes[i] == b'\n' {
                        out.push(Tok::Newline);
                    }
                    i += 1;
                }
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' => {
                i += 1;
                let mut text = String::new();
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 1;
                    }
                    text.push(bytes[i] as char);
                    i += 1;
                }
                i += 1;
                out.push(Tok::Str(text));
            }
            b'$' | b'@' | b'!' | b'%' => {
                let kind = match c {
                    b'$' => VarKind::Int,
                    b'@' => VarKind::Real,
                    b'!' => VarKind::Str,
                    _ => VarKind::Int,
                };
                let is_array = c == b'%';
                i += 1;
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let name = source[start..i].to_string();
                if is_array {
                    // `%name` is an int array; represent it as a var of Int kind that the runtime
                    // resolves to an array slot.
                    out.push(Tok::Var(VarKind::Int, format!("%{name}")));
                } else {
                    out.push(Tok::Var(kind, name));
                }
            }
            b'0'..=b'9' | b'.'
                if c.is_ascii_digit() || bytes.get(i + 1).is_some_and(|d| d.is_ascii_digit()) =>
            {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                    i += 1;
                }
                let text = &source[start..i];
                if let Ok(value) = text.parse::<i64>() {
                    out.push(Tok::Int(value));
                } else if let Ok(value) = text.parse::<f64>() {
                    out.push(Tok::Real(value));
                } else {
                    out.push(Tok::Int(0));
                }
            }
            b':' if bytes.get(i + 1) == Some(&b'=') => {
                out.push(Tok::Punct(":="));
                i += 2;
            }
            b'<' if bytes.get(i + 1) == Some(&b'=') => {
                out.push(Tok::Punct("<="));
                i += 2;
            }
            b'>' if bytes.get(i + 1) == Some(&b'=') => {
                out.push(Tok::Punct(">="));
                i += 2;
            }
            _ if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                out.push(Tok::Ident(source[start..i].to_string()));
            }
            _ => {
                let punct: &'static str = match c {
                    b'(' => "(",
                    b')' => ")",
                    b'[' => "[",
                    b']' => "]",
                    b',' => ",",
                    b'+' => "+",
                    b'-' => "-",
                    b'*' => "*",
                    b'/' => "/",
                    b'&' => "&",
                    b'=' => "=",
                    b'#' => "#",
                    b'<' => "<",
                    b'>' => ">",
                    _ => {
                        i += 1;
                        continue;
                    }
                };
                out.push(Tok::Punct(punct));
                i += 1;
            }
        }
    }
    out.push(Tok::Eof);
    out
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos]
    }

    fn next(&mut self) -> Tok {
        let tok = self.toks[self.pos].clone();
        self.pos = (self.pos + 1).min(self.toks.len() - 1);
        tok
    }

    fn eat_newlines(&mut self) {
        while matches!(self.peek(), Tok::Newline) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, punct: &str) -> bool {
        if matches!(self.peek(), Tok::Punct(p) if *p == punct) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, punct: &str) -> Result<(), String> {
        if self.eat(punct) {
            Ok(())
        } else {
            Err(format!("expected `{punct}`, found {:?}", self.peek()))
        }
    }

    fn expect_ident(&mut self) -> Result<String, String> {
        match self.next() {
            Tok::Ident(name) => Ok(name),
            other => Err(format!("expected identifier, found {other:?}")),
        }
    }

    fn program(&mut self) -> Result<Program, String> {
        let mut program = Program::default();
        loop {
            self.eat_newlines();
            match self.peek().clone() {
                Tok::Eof => break,
                Tok::Ident(word) if word == "on" => {
                    self.pos += 1;
                    let kind = self.expect_ident()?;
                    let arg = if self.eat("(") {
                        let name = match self.next() {
                            Tok::Var(_, name) => Some(name),
                            other => {
                                return Err(format!("expected callback argument, found {other:?}"));
                            }
                        };
                        self.expect(")")?;
                        name
                    } else {
                        None
                    };
                    let body = self.block(&["end"])?;
                    self.consume_terminator(&["end", "on"]);
                    program.callbacks.push(Callback { kind, arg, body });
                }
                Tok::Ident(word) if word == "function" => {
                    self.pos += 1;
                    let name = self.expect_ident()?;
                    // `function name` (no parameter list) is legal KSP and appears in retail
                    // scripts; the parenthesis is optional.
                    let mut params = Vec::new();
                    if !self.eat("(") {
                        let body = self.block(&["end", "function"])?;
                        self.consume_terminator(&["end", "function"]);
                        program.functions.insert(name, Function { params, body });
                        continue;
                    }
                    while !matches!(self.peek(), Tok::Punct(")")) {
                        let (kind, name) = match self.next() {
                            Tok::Var(kind, name) => (kind, name),
                            other => return Err(format!("expected parameter, found {other:?}")),
                        };
                        params.push((kind, name));
                        if !self.eat(",") {
                            break;
                        }
                    }
                    self.expect(")")?;
                    let body = self.block(&["end", "function"])?;
                    self.consume_terminator(&["end", "function"]);
                    program.functions.insert(name, Function { params, body });
                }
                other => return Err(format!("unexpected token at top level: {other:?}")),
            }
        }
        Ok(program)
    }

    /// Parse statements until one of `stop` appears (newlines skipped). The stop word is left for
    /// the caller to consume, because `end` is followed by a different word in every construct.
    fn block(&mut self, stop: &[&str]) -> Result<Vec<Stmt>, String> {
        let mut out = Vec::new();
        loop {
            self.eat_newlines();
            if stop.iter().any(|word| self.at_word(word)) {
                return Ok(out);
            }
            if matches!(self.peek(), Tok::Eof) {
                return Err(format!("unexpected end of input, expected one of {stop:?}"));
            }
            out.push(self.statement()?);
        }
    }

    fn at_word(&self, word: &str) -> bool {
        matches!(self.peek(), Tok::Ident(w) if w == word)
    }

    fn consume_terminator(&mut self, words: &[&str]) {
        for _ in words {
            self.pos += 1;
        }
    }

    fn statement(&mut self) -> Result<Stmt, String> {
        match self.peek().clone() {
            Tok::Ident(word) if word == "declare" => {
                self.pos += 1;
                self.declare()
            }
            Tok::Ident(word) if word == "if" => {
                self.pos += 1;
                self.if_statement()
            }
            Tok::Ident(word) if word == "while" => {
                self.pos += 1;
                let cond = self.expr()?;
                let body = self.block(&["end"])?;
                self.consume_terminator(&["end", "while"]);
                Ok(Stmt::While { cond, body })
            }
            Tok::Ident(word) if word == "select" => {
                self.pos += 1;
                self.select_statement()
            }
            Tok::Ident(word) if word == "exit" => {
                self.pos += 1;
                Ok(Stmt::Exit)
            }
            Tok::Ident(word) if word == "call" => {
                self.pos += 1;
                match self.expr()? {
                    expression @ Expr::Call(_, _) => Ok(Stmt::Expr(expression)),
                    _ => Err("expected a function name after call".to_string()),
                }
            }
            _ => {
                let lhs = self.expr()?;
                if self.eat(":=") {
                    let rhs = self.expr()?;
                    Ok(Stmt::Assign(lhs, rhs))
                } else {
                    Ok(Stmt::Expr(lhs))
                }
            }
        }
    }

    fn declare(&mut self) -> Result<Stmt, String> {
        // `declare const $x := 1` and `declare polyphonic $x` are attributes on the declaration.
        let mut polyphonic = false;
        while matches!(self.peek(), Tok::Ident(w) if w == "const" || w == "polyphonic" || w == "persistent")
        {
            polyphonic |= matches!(self.peek(), Tok::Ident(w) if w == "polyphonic");
            self.pos += 1;
        }
        if let Tok::Ident(keyword) = self.peek().clone() {
            let kind = match keyword.as_str() {
                "ui_knob" => Some(UiKind::Knob),
                "ui_slider" => Some(UiKind::Slider),
                "ui_label" => Some(UiKind::Label),
                "ui_menu" => Some(UiKind::Menu),
                "ui_switch" => Some(UiKind::Switch),
                "ui_button" => Some(UiKind::Button),
                "ui_value_edit" => Some(UiKind::ValueEdit),
                "ui_table" => Some(UiKind::Table),
                _ => None,
            };
            if let Some(kind) = kind {
                self.pos += 1;
                let name = match self.next() {
                    Tok::Var(_, name) => name,
                    other => return Err(format!("expected control name, found {other:?}")),
                };
                let len = if self.eat("[") {
                    let length = self.expr()?;
                    self.expect("]")?;
                    Some(length)
                } else {
                    None
                };
                let mut args = Vec::new();
                if self.eat("(") {
                    while !matches!(self.peek(), Tok::Punct(")")) {
                        args.push(self.expr()?);
                        if !self.eat(",") {
                            break;
                        }
                    }
                    self.expect(")")?;
                }
                return Ok(Stmt::DeclareUi(UiDecl {
                    kind,
                    name,
                    len,
                    args,
                }));
            }
        }
        let (kind, name) = match self.next() {
            Tok::Var(kind, name) => (kind, name),
            other => {
                return Err(format!(
                    "expected variable after `declare`, found {other:?}"
                ));
            }
        };
        let len = if self.eat("[") {
            let len = self.expr()?;
            self.expect("]")?;
            Some(len)
        } else {
            None
        };
        // An array may be initialised with a parenthesised list. A single element is also a
        // parenthesised expression, so only treat this as a list when the declaration has a length
        // and a comma shows up.
        let mut init = None;
        let mut array_init = None;
        if self.eat(":=") {
            if len.is_some() && matches!(self.peek(), Tok::Punct("(")) {
                let start = self.pos;
                self.pos += 1;
                // Long literals are written one row per line, so newlines are insignificant until
                // the closing parenthesis.
                self.eat_newlines();
                let mut values = vec![self.expr()?];
                let mut is_list = false;
                loop {
                    self.eat_newlines();
                    if !self.eat(",") {
                        break;
                    }
                    is_list = true;
                    self.eat_newlines();
                    values.push(self.expr()?);
                }
                self.eat_newlines();
                self.expect(")")?;
                if is_list {
                    array_init = Some(values);
                } else {
                    // `(expr)` is just a parenthesised initialiser; re-read it as one expression.
                    self.pos = start;
                    init = Some(self.expr()?);
                }
            } else {
                init = Some(self.expr()?);
            }
        }
        Ok(Stmt::Declare {
            polyphonic,
            kind,
            name,
            init,
            array_init,
            len,
        })
    }

    fn if_statement(&mut self) -> Result<Stmt, String> {
        let cond = self.expr()?;
        let mut arms = vec![(cond, self.block(&["else", "end"])?)];
        let mut otherwise = None;
        if self.at_word("else") {
            self.pos += 1;
            // `else if …` on one line is just an `if` as the first statement of the else branch,
            // which parses correctly and has the same meaning.
            otherwise = Some(self.block(&["end"])?);
        }
        self.eat_newlines();
        if !self.at_word("end") {
            return Err(format!("unterminated `if`, found {:?}", self.peek()));
        }
        self.consume_terminator(&["end", "if"]);
        let mut arms_out = Vec::new();
        std::mem::swap(&mut arms_out, &mut arms);
        Ok(Stmt::If {
            arms: arms_out,
            otherwise,
        })
    }

    fn select_statement(&mut self) -> Result<Stmt, String> {
        let parenthesised = self.eat("(");
        let subject = self.expr()?;
        if parenthesised {
            self.expect(")")?;
        }
        let mut cases = Vec::new();
        let mut otherwise = None;
        loop {
            self.eat_newlines();
            if self.at_word("end") {
                self.consume_terminator(&["end", "select"]);
                return Ok(Stmt::Select {
                    subject,
                    cases,
                    otherwise,
                });
            }
            match self.peek().clone() {
                Tok::Ident(w) if w == "case" => {
                    self.pos += 1;
                    let value = self.expr()?;
                    let body = self.select_block()?;
                    cases.push((value, body));
                }
                Tok::Ident(w) if w == "default" => {
                    self.pos += 1;
                    otherwise = Some(self.select_block()?);
                }
                other => return Err(format!("unexpected token in select: {other:?}")),
            }
        }
    }

    fn select_block(&mut self) -> Result<Vec<Stmt>, String> {
        self.block(&["case", "default", "end"])
    }

    fn expr(&mut self) -> Result<Expr, String> {
        self.or_expr()
    }

    fn or_expr(&mut self) -> Result<Expr, String> {
        let mut lhs = self.and_expr()?;
        while matches!(self.peek(), Tok::Ident(w) if w == "or") {
            self.pos += 1;
            let rhs = self.and_expr()?;
            lhs = Expr::Binary(BinOp::Or, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn and_expr(&mut self) -> Result<Expr, String> {
        let mut lhs = self.not_expr()?;
        while matches!(self.peek(), Tok::Ident(w) if w == "and") {
            self.pos += 1;
            let rhs = self.not_expr()?;
            lhs = Expr::Binary(BinOp::And, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn not_expr(&mut self) -> Result<Expr, String> {
        if matches!(self.peek(), Tok::Ident(w) if w == "not") {
            self.pos += 1;
            let inner = self.not_expr()?;
            return Ok(Expr::Unary(UnOp::Not, Box::new(inner)));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Expr, String> {
        let mut lhs = self.additive()?;
        loop {
            let op = match self.peek() {
                Tok::Punct("=") => BinOp::Eq,
                Tok::Punct("#") => BinOp::Ne,
                Tok::Punct("<") => BinOp::Lt,
                Tok::Punct("<=") => BinOp::Le,
                Tok::Punct(">") => BinOp::Gt,
                Tok::Punct(">=") => BinOp::Ge,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.additive()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn additive(&mut self) -> Result<Expr, String> {
        let mut lhs = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Tok::Punct("+") => BinOp::Add,
                Tok::Punct("-") => BinOp::Sub,
                Tok::Punct("&") => BinOp::Concat,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.multiplicative()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn multiplicative(&mut self) -> Result<Expr, String> {
        let mut lhs = self.unary()?;
        loop {
            let op = match self.peek() {
                Tok::Punct("*") => BinOp::Mul,
                Tok::Punct("/") => BinOp::Div,
                Tok::Ident(w) if w == "mod" => BinOp::Mod,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.unary()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Tok::Punct("-") => {
                self.pos += 1;
                Ok(Expr::Unary(UnOp::Neg, Box::new(self.unary()?)))
            }
            Tok::Punct("+") => {
                self.pos += 1;
                self.unary()
            }
            _ => self.postfix(),
        }
    }

    fn postfix(&mut self) -> Result<Expr, String> {
        let mut expr = self.primary()?;
        while self.eat("[") {
            let index = self.expr()?;
            self.expect("]")?;
            expr = Expr::Index(Box::new(expr), Box::new(index));
        }
        Ok(expr)
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.next() {
            Tok::Int(value) => Ok(Expr::Int(value)),
            Tok::Real(value) => Ok(Expr::Real(value)),
            Tok::Str(value) => Ok(Expr::Str(value)),
            Tok::Var(kind, name) => Ok(Expr::Var(kind, name)),
            Tok::Punct("(") => {
                let inner = self.expr()?;
                self.expect(")")?;
                Ok(inner)
            }
            Tok::Ident(name) => {
                if self.eat("(") {
                    let mut args = Vec::new();
                    while !matches!(self.peek(), Tok::Punct(")")) {
                        args.push(self.expr()?);
                        if !self.eat(",") {
                            break;
                        }
                    }
                    self.expect(")")?;
                    Ok(Expr::Call(name, args))
                } else {
                    // Bare identifiers are booleans in KSP (`true` / `false`).
                    match name.as_str() {
                        "true" => Ok(Expr::Int(1)),
                        "false" => Ok(Expr::Int(0)),
                        other => Ok(Expr::Call(other.to_string(), Vec::new())),
                    }
                }
            }
            other => Err(format!("unexpected token in expression: {other:?}")),
        }
    }
}

pub fn parse(source: &str) -> Result<Program, String> {
    let toks = lex(source);
    // Remember the source line of every token so an error can point at it.
    let mut lines = Vec::with_capacity(toks.len());
    let mut line = 1usize;
    for tok in &toks {
        lines.push(line);
        if matches!(tok, Tok::Newline) {
            line += 1;
        }
    }
    let source_lines: Vec<&str> = source.lines().collect();
    let mut parser = Parser { toks, pos: 0 };
    parser.program().map_err(|error| {
        let at = lines.get(parser.pos).copied().unwrap_or(line);
        let text = source_lines
            .get(at.saturating_sub(1))
            .map(|line| line.trim())
            .unwrap_or("");
        format!("line {at}: {error} — `{text}`")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ui_callbacks_and_select() {
        let program = parse(
            r#"
            on init
              declare $i := 0
              declare ui_slider $mic(0, 630000)
              make_persistent($mic)
              while $i < 3
                inc($i)
              end while
            end on

            on ui_control($mic)
              select($i)
                case 0
                  message("zero")
                case 1
                  message("one")
              end select
            end on
            "#,
        )
        .unwrap();
        assert_eq!(program.callbacks.len(), 2);
        assert_eq!(program.callbacks[0].kind, "init");
        assert_eq!(program.callbacks[1].arg.as_deref(), Some("mic"));
    }

    #[test]
    fn parses_array_initialisers() {
        let program = parse(
            r#"
            on init
              declare %interval[20] := (0, 0, 12, 19, 24)
              declare $scalar := (7)
            end on
            "#,
        )
        .unwrap();
        let body = &program.callbacks[0].body;
        match &body[0] {
            Stmt::Declare {
                array_init, init, ..
            } => {
                assert!(init.is_none());
                assert_eq!(array_init.as_ref().map(Vec::len), Some(5));
            }
            other => panic!("expected an array declaration, got {other:?}"),
        }
        // A single parenthesised value is just an initialiser, not a one-element list.
        match &body[1] {
            Stmt::Declare {
                array_init, init, ..
            } => {
                assert!(array_init.is_none());
                assert!(init.is_some());
            }
            other => panic!("expected a scalar declaration, got {other:?}"),
        }
    }

    /// `function name` without a parameter list is legal KSP, and `end function` closes it.
    #[test]
    fn parses_a_function_without_a_parameter_list() {
        let program = parse(
            r#"
            function all_off1
              $button := 0
            end function

            on init
              call all_off1
            end on
            "#,
        )
        .unwrap();
        assert!(program.functions.contains_key("all_off1"));
        assert!(program.functions["all_off1"].params.is_empty());
        assert_eq!(program.callbacks.len(), 1);
        assert!(
            matches!(&program.callbacks[0].body[0], Stmt::Expr(Expr::Call(name, args))
            if name == "all_off1" && args.is_empty())
        );
        assert_eq!(program.callbacks[0].body.len(), 1);
    }

    /// Retail libraries write long tables one row per line and put `...` where the row continues.
    /// Kontakt accepts it; one such table must not take the whole script slot down.
    #[test]
    fn parses_a_multi_line_table_with_ellipsis_rows() {
        let program = parse(
            r#"
            on init
              declare %sine_vels[6] := ( ...
            ...
            {+1} 1,2,3,...
            {+2} 4,5,6)
            end on
            "#,
        )
        .unwrap();
        match &program.callbacks[0].body[0] {
            Stmt::Declare { array_init, .. } => {
                assert_eq!(array_init.as_ref().map(Vec::len), Some(6));
            }
            other => panic!("expected an array declaration, got {other:?}"),
        }
    }
}
