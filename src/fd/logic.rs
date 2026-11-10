//! The `<logic>` block (SPEC 3.4): lines `tag = expression[,delay:N]`, parsed once at load, evaluated every scan cycle
//! against the values the store held when the cycle began. Results go to the updated-tag queue, never straight into
//! the store, so no line sees an earlier line's result in the same cycle.

use super::config::{ConfigError, Opts};
use super::store::Store;
use crate::hint::show;
use crate::model::{Kind, Value};
use std::collections::HashMap;

type Res<T> = Result<T, String>;
type BinFn = fn(f64, f64) -> Res<f64>;
type UnFn = fn(f64) -> f64;
type CallFn = fn(&[f64]) -> f64;

fn truthy(x: f64) -> bool {
    x != 0.0
}

fn flag(b: bool) -> f64 {
    f64::from(u8::from(b))
}

/// Stock `&&` / `||` cut both operands to int64 first, so `0.5 && 1` is 0. `--strict` uses `!= 0`.
fn cut(x: f64) -> bool {
    x as i64 != 0
}

fn int_op(a: f64, b: f64, f: fn(i64, i64) -> Option<i64>) -> Res<f64> {
    f(a as i64, b as i64)
        .map(|x| x as f64)
        .ok_or_else(|| "integer operation out of range".into())
}

fn shift(x: i64, n: i64) -> Option<i64> {
    u32::try_from(n).ok().and_then(|n| x.checked_shl(n))
}

fn shift_right(x: i64, n: i64) -> Option<i64> {
    u32::try_from(n).ok().and_then(|n| x.checked_shr(n))
}

/// Binary operators, one per spelling. Looked up at parse time, so a bad operator is a load error.
const BINARY: &[(&str, BinFn)] = &[
    ("||", |a, b| Ok(flag(cut(a) || cut(b)))),
    ("&&", |a, b| Ok(flag(cut(a) && cut(b)))),
    ("==", |a, b| Ok(flag(a == b))),
    ("!=", |a, b| Ok(flag(a != b))),
    ("<", |a, b| Ok(flag(a < b))),
    ("<=", |a, b| Ok(flag(a <= b))),
    (">", |a, b| Ok(flag(a > b))),
    (">=", |a, b| Ok(flag(a >= b))),
    ("<<", |a, b| int_op(a, b, shift)),
    (">>", |a, b| int_op(a, b, shift_right)),
    ("+", |a, b| Ok(a + b)),
    ("-", |a, b| Ok(a - b)),
    ("*", |a, b| Ok(a * b)),
    ("/", |a, b| Ok(a / b)),
    ("%", |a, b| int_op(a, b, i64::checked_rem)),
    ("**", |a, b| Ok(a.powf(b))),
];

const STRICT: &[(&str, BinFn)] = &[
    ("||", |a, b| Ok(flag(truthy(a) || truthy(b)))),
    ("&&", |a, b| Ok(flag(truthy(a) && truthy(b)))),
];

/// Binding levels, loosest first; `**` and the prefix operators bind tightest and associate to the left.
const LEVELS: &[&[&str]] = &[
    &["||"],
    &["&&"],
    &["==", "!="],
    &["<", "<=", ">", ">="],
    &["<<", ">>"],
    &["+", "-"],
    &["*", "/", "%"],
];

const UNARY: &[(&str, UnFn)] = &[("-", |x| -x), ("+", |x| x), ("!", |x| flag(!truthy(x)))];

type Func = (&'static str, usize, CallFn);
const FUNCS: &[Func] = &[
    ("sin", 1, |a| a[0].sin()),
    ("cos", 1, |a| a[0].cos()),
    ("tan", 1, |a| a[0].tan()),
    ("abs", 1, |a| a[0].abs()),
    ("sqrt", 1, |a| a[0].sqrt()),
    ("int", 1, |a| a[0].trunc()),
    ("float", 1, |a| a[0]),
    ("pow", 2, |a| a[0].powf(a[1])),
];

// ---- tokens --------------------------------------------------------------------------------------------------

#[derive(Debug, PartialEq, Clone)]
enum Tok {
    Num(f64),
    Tag(String),
    Ident(String),
    Op(&'static str),
    Open,
    Close,
    Comma,
}

const OPS: &[&str] = &[
    "**", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+", "-", "*", "/", "%", "<", ">", "!",
];

fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Tag names, longest first, so `var_I10` wins over `var_I1`.
struct Names<'a>(Vec<&'a str>);

impl<'a> Names<'a> {
    fn new(tags: &'a HashMap<String, Kind>) -> Names<'a> {
        let mut v: Vec<&str> = tags.keys().map(String::as_str).collect();
        v.sort_by_key(|n| std::cmp::Reverse(n.len()));
        Names(v)
    }

    /// A tag name at the start of `s`, ending at an identifier boundary.
    fn at(&self, s: &str) -> Option<&'a str> {
        self.0.iter().copied().find(|n| {
            s.strip_prefix(n)
                .is_some_and(|r| !r.starts_with(ident_char) || !n.ends_with(ident_char))
        })
    }
}

fn number(s: &str) -> Option<(Tok, usize)> {
    let digits = |s: &str| {
        s.chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .count()
    };
    let mut n = digits(s);
    if n == 0 || s.starts_with('.') && n == 1 {
        return None;
    }
    if let Some(e) = s[n..].strip_prefix(['e', 'E']) {
        let sign = usize::from(e.starts_with(['+', '-']));
        let d = e[sign..].chars().take_while(char::is_ascii_digit).count();
        if d > 0 {
            n += 1 + sign + d;
        }
    }
    s[..n].parse().ok().map(|x| (Tok::Num(x), n))
}

fn ident(s: &str) -> Option<(Tok, usize)> {
    let n: usize = s
        .chars()
        .take_while(|c| ident_char(*c))
        .map(char::len_utf8)
        .sum();
    (n > 0).then(|| (Tok::Ident(s[..n].into()), n))
}

fn punct(s: &str) -> Option<(Tok, usize)> {
    if let Some(op) = OPS.iter().find(|o| s.starts_with(**o)) {
        return Some((Tok::Op(op), op.len()));
    }
    let t = match s.chars().next()? {
        '(' => Tok::Open,
        ')' => Tok::Close,
        ',' => Tok::Comma,
        _ => return None,
    };
    Some((t, 1))
}

fn tokens(src: &str, names: &Names) -> Res<Vec<Tok>> {
    let (mut out, mut rest) = (vec![], src.trim_start());
    while !rest.is_empty() {
        let tagged = names.at(rest).map(|n| (Tok::Tag(n.into()), n.len()));
        let (tok, len) = tagged
            .or_else(|| number(rest))
            .or_else(|| ident(rest))
            .or_else(|| punct(rest))
            .ok_or_else(|| format!("unexpected `{}`", show(rest)))?;
        out.push(tok);
        rest = rest[len..].trim_start();
    }
    Ok(out)
}

// ---- expressions ---------------------------------------------------------------------------------------------

enum Expr {
    Num(f64),
    Tag(String),
    Unary(UnFn, Box<Expr>),
    Bin(BinFn, Box<Expr>, Box<Expr>),
    Call(CallFn, Vec<Expr>),
}

impl Expr {
    fn eval(&self, store: &Store) -> Res<f64> {
        match self {
            Expr::Num(x) => Ok(*x),
            Expr::Tag(t) => match store.get(t) {
                Some(Value::Analog(x)) => Ok(x),
                Some(Value::Binary(b)) => Ok(flag(b)),
                None => Err(format!("unknown tag `{t}`")),
            },
            Expr::Unary(f, e) => Ok(f(e.eval(store)?)),
            Expr::Bin(f, a, b) => f(a.eval(store)?, b.eval(store)?),
            Expr::Call(f, args) => {
                let v: Res<Vec<f64>> = args.iter().map(|a| a.eval(store)).collect();
                Ok(f(&v?))
            }
        }
    }
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    strict: bool,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += usize::from(t.is_some());
        t
    }

    fn eat(&mut self, want: &Tok) -> bool {
        let hit = self.peek() == Some(want);
        self.pos += usize::from(hit);
        hit
    }

    /// The operator at the cursor if it is one of `ops`.
    fn eat_op(&mut self, ops: &[&'static str]) -> Option<&'static str> {
        let Some(Tok::Op(o)) = self.peek() else {
            return None;
        };
        let o = ops.iter().copied().find(|x| x == o)?;
        self.pos += 1;
        Some(o)
    }

    fn level(&mut self, lvl: usize) -> Res<Expr> {
        let Some(ops) = LEVELS.get(lvl) else {
            return self.power();
        };
        let mut lhs = self.level(lvl + 1)?;
        while let Some(op) = self.eat_op(ops) {
            let rhs = self.level(lvl + 1)?;
            lhs = Expr::Bin(self.binary(op), Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn binary(&self, op: &str) -> BinFn {
        let strict = STRICT.iter().filter(|_| self.strict);
        strict
            .chain(BINARY)
            .find(|(n, _)| *n == op)
            .map_or(BINARY[0].1, |(_, f)| *f) // every spelling in LEVELS and `**` is in BINARY
    }

    fn power(&mut self) -> Res<Expr> {
        let mut lhs = self.prefix()?;
        while self.eat_op(&["**"]).is_some() {
            let rhs = self.prefix()?;
            lhs = Expr::Bin(self.binary("**"), Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn prefix(&mut self) -> Res<Expr> {
        if let Some(op) = self.eat_op(&["-", "+", "!"]) {
            let f = UNARY
                .iter()
                .find(|(n, _)| *n == op)
                .map_or(UNARY[1].1, |u| u.1);
            return Ok(Expr::Unary(f, Box::new(self.prefix()?)));
        }
        self.atom()
    }

    fn atom(&mut self) -> Res<Expr> {
        match self.next() {
            Some(Tok::Num(x)) => Ok(Expr::Num(x)),
            Some(Tok::Tag(t)) => Ok(Expr::Tag(t)),
            Some(Tok::Open) => {
                let e = self.level(0)?;
                self.eat(&Tok::Close)
                    .then_some(e)
                    .ok_or_else(|| "missing `)`".into())
            }
            Some(Tok::Ident(name)) => self.named(&name),
            Some(t) => Err(format!("unexpected {t:?}")),
            None => Err("expression ends early".into()),
        }
    }

    /// `True` / `False` or a function call.
    fn named(&mut self, name: &str) -> Res<Expr> {
        match name {
            "True" => return Ok(Expr::Num(1.0)),
            "False" => return Ok(Expr::Num(0.0)),
            _ => {}
        }
        let &(_, arity, f) = FUNCS
            .iter()
            .find(|f| f.0 == name)
            .ok_or_else(|| format!("unknown name `{}`", show(name)))?;
        let mut args = vec![];
        if !self.eat(&Tok::Open) {
            return Err(format!("`{name}` needs `(`"));
        }
        loop {
            args.push(self.level(0)?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        if !self.eat(&Tok::Close) {
            return Err("missing `)`".into());
        }
        match args.len() == arity {
            true => Ok(Expr::Call(f, args)),
            false => Err(format!("`{name}` takes {arity} argument(s)")),
        }
    }
}

fn parse_expr(src: &str, names: &Names, strict: bool) -> Res<Expr> {
    let mut p = Parser {
        toks: tokens(src, names)?,
        pos: 0,
        strict,
    };
    let e = p.level(0)?;
    match p.peek() {
        None => Ok(e),
        Some(t) => Err(format!("unexpected {t:?} after the expression")),
    }
}

// ---- lines ---------------------------------------------------------------------------------------------------

struct Line {
    lhs: String,
    kind: Kind,
    expr: Expr,
    /// Cycles a changed result must wait before it is written.
    delay: i64,
    /// Milliseconds left on the running delay.
    timer: Option<i64>,
}

pub struct Logic(Vec<Line>);

fn strip_comments(text: &str) -> String {
    let (mut out, mut rest) = (String::new(), text);
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 2..];
        let (body, after) = tail.split_once("*/").unwrap_or((tail, ""));
        out.extend(body.chars().filter(|c| *c == '\n')); // keep the line numbers
        rest = after;
    }
    out.push_str(rest);
    out.lines()
        .map(|l| {
            &l[..["#", "//"]
                .iter()
                .filter_map(|m| l.find(m))
                .min()
                .unwrap_or(l.len())]
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `rhs,delay:N` -> (`rhs`, N).
fn split_delay(rhs: &str) -> Res<(&str, i64)> {
    let Some((head, tail)) = rhs.rsplit_once(',') else {
        return Ok((rhs, 0));
    };
    match tail.trim().strip_prefix("delay:") {
        Some(n) => {
            let n = n.trim().parse().ok().filter(|n| *n >= 0);
            Ok((head, n.ok_or("delay needs a whole number of cycles")?))
        }
        None => Ok((rhs, 0)),
    }
}

impl Logic {
    /// Compile `text` against the device's tags. A syntax error names the line; a line assigning an unknown tag is a warning (an error under `--strict`) and is skipped.
    pub fn parse(
        text: &str,
        tags: &HashMap<String, Kind>,
        opts: &Opts,
    ) -> Result<Logic, ConfigError> {
        let names = Names::new(tags);
        let mut lines = vec![];
        for (n, raw) in strip_comments(text).lines().enumerate() {
            if raw.trim().is_empty() {
                continue;
            }
            let at = |e: String| ConfigError(format!("logic line {}: {e}", n + 1));
            let (lhs, rhs) = raw
                .split_once('=')
                .ok_or_else(|| at("expected `tag = expression`".into()))?;
            let lhs = lhs.trim();
            let Some(&kind) = tags.get(lhs) else {
                opts.warn(format!(
                    "logic line {}: `{}` is not a known tag, line skipped",
                    n + 1,
                    show(lhs)
                ))?;
                continue;
            };
            let (rhs, delay) = split_delay(rhs).map_err(at)?;
            let expr = parse_expr(rhs, &names, opts.strict).map_err(at)?;
            lines.push(Line {
                lhs: lhs.into(),
                kind,
                expr,
                delay,
                timer: None,
            });
        }
        Ok(Logic(lines))
    }

    /// One scan cycle: queue every changed result. Returns the log lines to print.
    pub fn scan(&mut self, store: &Store, cycle_ms: u64) -> Vec<String> {
        let cycle = i64::try_from(cycle_ms).unwrap_or(i64::MAX);
        let mut notes = vec![];
        for line in &mut self.0 {
            line.step(store, cycle, &mut notes);
        }
        notes
    }
}

impl Line {
    fn result(&self, store: &Store) -> Res<Value> {
        let x = self.expr.eval(store)?;
        match (self.kind, x.is_nan()) {
            (_, true) => Err("result is not a number".into()),
            (Kind::Binary, _) => Ok(Value::Binary(truthy(x))),
            (Kind::Analog, _) => Ok(Value::Analog(x)),
        }
    }

    /// Advance the delay timer, then queue the result if it differs from the stored value (SPEC 3.4 `delay:N` state machine).
    fn step(&mut self, store: &Store, cycle: i64, notes: &mut Vec<String>) {
        let expired = self.timer.is_some();
        if let Some(left) = self.timer.map(|t| t - cycle) {
            if left > 0 {
                self.timer = Some(left);
                notes.push(format!("[ DELAYED {left}ms] {}", self.lhs));
                return;
            }
            self.timer = None;
        }
        if store.is_queued(&self.lhs) {
            return;
        }
        let new = match self.result(store) {
            Ok(v) => v,
            Err(e) => return notes.push(format!("logic `{}`: {e}", self.lhs)),
        };
        if store.get(&self.lhs) == Some(new) {
            return;
        }
        if self.delay > 0 && !expired {
            self.timer = Some(self.delay * cycle);
            notes.push(format!(
                "Delaying tag: {} for {}ms",
                self.lhs,
                self.delay * cycle
            ));
        } else {
            let _ = store.write(&self.lhs, new); // kind matches by construction
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd::store::Layout;

    const SENTINEL: f64 = 0.123;

    /// A store with analog internal tags x=2, y=3, var_I1=10, var_I10=20, out=SENTINEL, binary flag=false, and their kinds.
    fn world() -> (Store, HashMap<String, Kind>) {
        let l = Layout {
            points: vec![],
            external: vec![],
            internal: vec![
                ("x".into(), Value::Analog(2.0)),
                ("y".into(), Value::Analog(3.0)),
                ("var_I1".into(), Value::Analog(10.0)),
                ("var_I10".into(), Value::Analog(20.0)),
                ("lock.height".into(), Value::Analog(4.0)),
                ("out".into(), Value::Analog(SENTINEL)),
                ("flag".into(), Value::Binary(false)),
                ("on".into(), Value::Binary(true)),
            ],
        };
        let kinds = l
            .internal
            .iter()
            .map(|(t, v)| {
                (
                    t.clone(),
                    if matches!(v, Value::Binary(_)) {
                        Kind::Binary
                    } else {
                        Kind::Analog
                    },
                )
            })
            .collect();
        (Store::new(l), kinds)
    }

    fn compile(text: &str, strict: bool) -> Result<Logic, ConfigError> {
        Logic::parse(text, &world().1, &Opts { strict })
    }

    /// Run `text` for one cycle on a fresh world; `out` afterwards.
    fn run(text: &str, strict: bool) -> (f64, Vec<String>) {
        let (store, kinds) = world();
        let mut l = Logic::parse(text, &kinds, &Opts { strict }).unwrap();
        let notes = l.scan(&store, 100);
        store.apply_internal();
        (store.get_f64("out").unwrap(), notes)
    }

    fn eval(expr: &str) -> f64 {
        run(&format!("out = {expr}"), false).0
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(eval("1 + 2 * 3"), 7.0);
        assert_eq!(eval("(1 + 2) * 3"), 9.0);
        assert_eq!(eval("10 - 4 - 3"), 3.0);
        assert_eq!(eval("8 / 4 / 2"), 1.0);
        assert_eq!(eval("1.5e1 + .5 + 2."), 17.5);
        assert_eq!(eval("2E+1 - 1e-1"), 19.9);
    }

    #[test]
    fn stock_observations() {
        assert_eq!(eval("7 % 3"), 1.0);
        assert_eq!(eval("2 ** 3 ** 2"), 64.0);
        assert_eq!(eval("-2 ** 2"), 4.0);
        assert_eq!(eval("2 ** -1"), 0.5);
        assert_eq!(eval("1 / 0"), f64::INFINITY);
        assert_eq!(eval("0.0 == False"), 1.0);
        assert_eq!(eval("0.5 && 1"), 0.0);
        assert_eq!(eval("0.4 || 0"), 0.0);
        assert_eq!(eval("!0.5"), 0.0);
        assert_eq!(eval("!0"), 1.0);
        assert_eq!(eval("sqrt(16) + pow(2,10)"), 1028.0);
    }

    #[test]
    fn strict_uses_not_equal_zero() {
        assert_eq!(run("out = 0.5 && 1", true).0, 1.0);
        assert_eq!(run("out = 0.4 || 0", true).0, 1.0);
        assert_eq!(run("out = 0 || 0", true).0, 0.0);
        assert_eq!(run("out = 1 && 0", true).0, 0.0);
    }

    #[test]
    fn comparison_logic_and_shift_levels() {
        assert_eq!(eval("1 << 2 + 1"), 8.0);
        assert_eq!(eval("256 >> 4"), 16.0);
        assert_eq!(eval("1 < 2 == 1"), 1.0);
        assert_eq!(eval("1 || 0 && 0"), 1.0);
        assert_eq!(eval("2 * 3 == 6 && 1"), 1.0);
        assert_eq!(eval("(1 || 0) && 0"), 0.0);
        for (e, want) in [
            ("1 < 2", 1.0),
            ("2 < 2", 0.0),
            ("2 <= 2", 1.0),
            ("3 <= 2", 0.0),
            ("3 > 2", 1.0),
            ("2 > 2", 0.0),
            ("2 >= 2", 1.0),
            ("1 >= 2", 0.0),
            ("1 != 2", 1.0),
            ("2 != 2", 0.0),
            ("2 == 2", 1.0),
            ("1 == 2", 0.0),
        ] {
            assert_eq!(eval(e), want, "{e}");
        }
        assert_eq!(eval("+5 - -3"), 8.0);
        assert_eq!(eval("1 && 1"), 1.0);
        assert_eq!(eval("0 && 1"), 0.0);
        assert_eq!(eval("1 || 0"), 1.0);
        assert_eq!(eval("0 || 0"), 0.0);
    }

    #[test]
    fn functions() {
        assert_eq!(eval("abs(0 - 3)"), 3.0);
        assert_eq!(eval("int(2.7)"), 2.0);
        assert_eq!(eval("float(2.5)"), 2.5);
        assert!((eval("sin(0) + cos(0)") - 1.0).abs() < 1e-12);
        assert!((eval("tan(0.5)") - 0.5f64.tan()).abs() < 1e-12);
        assert_eq!(eval("pow(x, y)"), 8.0);
    }

    #[test]
    fn tags_are_matched_longest_first_at_boundaries() {
        assert_eq!(eval("var_I10 + var_I1"), 30.0);
        assert_eq!(eval("var_I1+x"), 12.0);
        assert_eq!(eval("lock.height * 2"), 8.0);
        assert_eq!(eval("on + flag + True + False"), 2.0);
        let e = compile("out = x1", false).err().unwrap();
        assert!(e.0.contains("logic line 1: unknown name `x1`"), "{e}");
    }

    #[test]
    fn line_errors_name_the_line() {
        for (text, want) in [
            (
                "out = 1\nout 1",
                "logic line 2: expected `tag = expression`",
            ),
            ("out = (1 + 2", "logic line 1: missing `)`"),
            ("out = 1 +", "logic line 1: expression ends early"),
            (
                "out = 1 2",
                "logic line 1: unexpected Num(2.0) after the expression",
            ),
            ("out = $", "logic line 1: unexpected `$`"),
            ("out = foo(1)", "unknown name `foo`"),
            ("out = sqrt", "`sqrt` needs `(`"),
            ("out = sqrt(1, 2)", "`sqrt` takes 1 argument(s)"),
            ("out = pow(1)", "`pow` takes 2 argument(s)"),
            ("out = sqrt(4", "missing `)`"),
            ("out = 1, delay:x", "delay needs a whole number"),
            ("out = 1, delay:-2", "delay needs a whole number"),
            ("out = )", "unexpected Close"),
            ("out = .", "unexpected `.`"),
            ("out = 1.2.3", "unexpected `.2.3`"),
            ("out = 1e", "unexpected Ident(\"e\")"),
        ] {
            let e = compile(text, false)
                .err()
                .unwrap_or_else(|| panic!("{text} compiled"));
            assert!(e.0.contains(want), "{text}: {e}");
        }
    }

    #[test]
    fn comments_blank_lines_and_assignment_split() {
        let t = "# header\nout = 1 // trailing\n\n/* multi\nline */ out = 2 # again\n";
        assert_eq!(run(t, false).0, 1.0); // out = 1 is queued first, out = 2 is skipped
        assert_eq!(run("out = 1 /* x */ + 2", false).0, 3.0);
        assert_eq!(run("out = 1 /* open", false).0, 1.0);
        assert_eq!(run("out = x == 2", false).0, 1.0); // split on the first `=` only
        let e = compile("/* a\nb */\nbad", false).err().unwrap();
        assert!(e.0.contains("logic line 3"), "{e}");
    }

    #[test]
    fn unknown_lhs_warns_or_fails_under_strict() {
        assert_eq!(run("nope = 1\nout = 2", false).0, 2.0);
        let e = compile("nope = 1", true).err().unwrap();
        assert!(
            e.0.contains("logic line 1: `nope` is not a known tag"),
            "{e}"
        );
    }

    #[test]
    fn lines_read_the_store_not_each_others_results() {
        let (store, kinds) = world();
        let mut l = Logic::parse("x = 5\ny = x", &kinds, &Opts::default()).unwrap();
        l.scan(&store, 100);
        store.apply_internal();
        assert_eq!(
            (store.get_f64("x"), store.get_f64("y")),
            (Some(5.0), Some(2.0))
        );
    }

    #[test]
    fn only_changes_are_queued_and_an_earlier_line_wins() {
        let (store, kinds) = world();
        let mut l = Logic::parse(
            "x = 2\nout = 1\nout = 2\nflag = 5\non = 0",
            &kinds,
            &Opts::default(),
        )
        .unwrap();
        l.scan(&store, 100);
        assert!(!store.is_queued("x"));
        store.apply_internal();
        assert_eq!(store.get_f64("out"), Some(1.0));
        assert_eq!(
            (store.get_bool("flag"), store.get_bool("on")),
            (Some(true), Some(false))
        );
    }

    #[test]
    fn bad_results_leave_the_tag_and_say_so() {
        for e in ["5 % 0", "1 << 64", "1 << -1", "sqrt(0 - 1)"] {
            let (v, notes) = run(&format!("out = {e}"), false);
            assert_eq!(v, SENTINEL, "{e}");
            assert_eq!(notes.len(), 1, "{e}");
            assert!(notes[0].starts_with("logic `out`: "), "{notes:?}");
        }
        assert_eq!(run("out = 5 % 3 % 2", false).0, 0.0);
        assert_eq!(run("out = 1 << 62 >> 61", false).0, 2.0);
    }

    #[test]
    fn delay_waits_n_cycles_then_writes_if_still_different() {
        let (store, kinds) = world();
        let mut l = Logic::parse("out = x, delay:3", &kinds, &Opts::default()).unwrap();
        let n = l.scan(&store, 200);
        assert_eq!(n, ["Delaying tag: out for 600ms"]);
        assert!(!store.is_queued("out"));
        assert_eq!(l.scan(&store, 200), ["[ DELAYED 400ms] out"]);
        assert_eq!(l.scan(&store, 200), ["[ DELAYED 200ms] out"]);
        assert!(!store.is_queued("out"));
        assert!(l.scan(&store, 200).is_empty());
        assert!(store.is_queued("out"));
        store.apply_internal();
        assert_eq!(store.get_f64("out"), Some(2.0));
        assert!(l.scan(&store, 200).is_empty()); // settled
    }

    #[test]
    fn delay_is_cancelled_when_the_result_goes_back() {
        let (store, kinds) = world();
        let mut l = Logic::parse("out = x, delay:1", &kinds, &Opts::default()).unwrap();
        l.scan(&store, 100); // timer 100
        store.write("x", Value::Analog(SENTINEL)).unwrap();
        store.apply_internal(); // x == out now
        assert!(l.scan(&store, 100).is_empty());
        assert!(!store.is_queued("out"));
        assert!(l.scan(&store, 100).is_empty()); // timer is gone: nothing pending, nothing delayed
    }

    #[test]
    fn an_expired_delay_does_not_start_a_second_one() {
        let (store, kinds) = world();
        let mut l = Logic::parse("out = x, delay:1", &kinds, &Opts::default()).unwrap();
        l.scan(&store, 100);
        store.write("x", Value::Analog(7.0)).unwrap();
        store.apply_internal();
        l.scan(&store, 100); // timer expired, result differs from out: written now, with the new value
        store.apply_internal();
        assert_eq!(store.get_f64("out"), Some(7.0));
    }

    #[test]
    fn a_queued_tag_is_left_alone_even_with_a_delay_running() {
        let (store, kinds) = world();
        let mut l = Logic::parse("out = x, delay:2", &kinds, &Opts::default()).unwrap();
        l.scan(&store, 100);
        store.write("out", Value::Analog(9.0)).unwrap();
        l.scan(&store, 100);
        assert!(l.scan(&store, 100).is_empty()); // timer expired, tag queued by someone else: skipped
        store.apply_internal();
        assert_eq!(store.get_f64("out"), Some(9.0));
    }
}
