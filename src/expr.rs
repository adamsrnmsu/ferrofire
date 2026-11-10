//! Tiny expression language for model files (spec section 3.1). Pratt parser -> tree, resolved to value slots at
//! compile time; evaluation is allocation-free.
//!
//! numbers, identifiers `[A-Za-z_][A-Za-z0-9_.]*` or back-quoted tags (`` `bus-3`.voltage ``), `+ - * / % ^`,
//! unary `- !`, `< <= > >= == !=` (1/0), `&& ||`, `c ? a : b`, `if(c,a,b)`,
//! functions `min max abs sqrt exp ln pow clamp sign step floor`.

use crate::hint::did_you_mean;
use std::collections::HashMap;

/// A binary operator, looked up once at parse time.
pub type BinFn = fn(f64, f64) -> f64;
/// A function of up to three arguments (unused slots are 0).
pub type Func = fn(&[f64; 3]) -> f64;

#[derive(Clone, Debug)]
pub enum Node {
    Num(f64),
    Var(usize),
    Neg(Box<Node>),
    Not(Box<Node>),
    Bin(BinFn, Box<Node>, Box<Node>),
    Tern(Box<Node>, Box<Node>, Box<Node>),
    Call(Func, Vec<Node>),
}

fn b(x: bool) -> f64 {
    x as u8 as f64
}

/// Functions by name: (name, arity, implementation).
const FUNCS: [(&str, usize, Func); 11] = [
    ("min", 2, |a| a[0].min(a[1])),
    ("max", 2, |a| a[0].max(a[1])),
    ("abs", 1, |a| a[0].abs()),
    ("sqrt", 1, |a| a[0].sqrt()),
    ("exp", 1, |a| a[0].exp()),
    ("ln", 1, |a| a[0].ln()),
    ("pow", 2, |a| a[0].powf(a[1])),
    ("clamp", 3, |a| a[0].max(a[1]).min(a[2])), // clamp(x, lo, hi)
    ("sign", 1, |a| b(a[0] > 0.0) - b(a[0] < 0.0)),
    ("step", 1, |a| b(a[0] >= 0.0)),
    ("floor", 1, |a| a[0].floor()),
];

/// Binary operators: (symbol, precedence, right-associative, implementation).
const BINDINGS: [(&str, u8, bool, BinFn); 14] = [
    ("||", 2, false, |x, y| b(x != 0.0 || y != 0.0)),
    ("&&", 3, false, |x, y| b(x != 0.0 && y != 0.0)),
    ("==", 4, false, |x, y| b(x == y)),
    ("!=", 4, false, |x, y| b(x != y)),
    ("<", 5, false, |x, y| b(x < y)),
    ("<=", 5, false, |x, y| b(x <= y)),
    (">", 5, false, |x, y| b(x > y)),
    (">=", 5, false, |x, y| b(x >= y)),
    ("+", 6, false, |x, y| x + y),
    ("-", 6, false, |x, y| x - y),
    ("*", 7, false, |x, y| x * y),
    ("/", 7, false, |x, y| x / y),
    ("%", 7, false, |x, y| x % y),
    ("^", 9, true, f64::powf),
];

#[derive(Debug)]
pub struct ParseError {
    pub pos: usize,
    pub msg: String,
}
impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "at column {}: {}", self.pos + 1, self.msg)
    }
}

fn err_at(pos: usize, msg: impl Into<String>) -> ParseError {
    ParseError {
        pos,
        msg: msg.into(),
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Sym(&'static str),
    End,
}

const SYMS: [&str; 20] = [
    "<=", ">=", "==", "!=", "&&", "||", "+", "-", "*", "/", "%", "^", "<", ">", "!", "?", ":", "(",
    ")", ",",
];

/// First index at or after `i` whose byte fails `keep`.
fn skip(b: &[u8], mut i: usize, keep: impl Fn(u8) -> bool) -> usize {
    while i < b.len() && keep(b[i]) {
        i += 1;
    }
    i
}

/// End of an optional exponent (`e`, `E`, optional sign, digits) starting at `i`; `i` itself when there is none.
fn exponent_end(b: &[u8], i: usize) -> usize {
    if !matches!(b.get(i), Some(b'e' | b'E')) {
        return i;
    }
    let mut j = i + 1;
    if matches!(b.get(j), Some(b'+' | b'-')) {
        j += 1;
    }
    let k = skip(b, j, |c| c.is_ascii_digit());
    if k > j {
        k
    } else {
        i
    }
}

fn starts_number(b: &[u8], i: usize) -> bool {
    b[i].is_ascii_digit() || (b[i] == b'.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
}

fn lex_number(src: &str, start: usize) -> Result<(Tok, usize), ParseError> {
    let b = src.as_bytes();
    let end = exponent_end(b, skip(b, start, |c| c.is_ascii_digit() || c == b'.'));
    let v = src[start..end]
        .parse::<f64>()
        .map_err(|_| err_at(start, format!("bad number `{}`", &src[start..end])))?;
    Ok((Tok::Num(v), end))
}

fn starts_ident(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '`'
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'.'
}

/// Identifier: runs of [A-Za-z0-9_.] and back-quoted segments (tags containing `-`).
fn lex_ident(src: &str, start: usize) -> Result<(Tok, usize), ParseError> {
    let b = src.as_bytes();
    let (mut name, mut i) = (String::new(), start);
    while i < b.len() {
        if b[i] == b'`' {
            let end = src[i + 1..]
                .find('`')
                .ok_or_else(|| err_at(i, "unterminated back-quote"))?;
            name.push_str(&src[i + 1..i + 1 + end]);
            i += end + 2;
        } else if is_ident_byte(b[i]) {
            name.push(b[i] as char);
            i += 1;
        } else {
            break;
        }
    }
    Ok((Tok::Ident(name), i))
}

fn lex_symbol(src: &str, i: usize) -> Result<(Tok, usize), ParseError> {
    let s = SYMS
        .iter()
        .find(|s| src[i..].starts_with(**s))
        .ok_or_else(|| {
            err_at(
                i,
                format!("unexpected `{}`", src[i..].chars().next().unwrap()),
            )
        })?;
    Ok((Tok::Sym(s), i + s.len()))
}

fn lex(src: &str) -> Result<Vec<(Tok, usize)>, ParseError> {
    let b = src.as_bytes();
    let (mut i, mut out) = (0, vec![]);
    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let (tok, next) = if starts_number(b, i) {
            lex_number(src, i)?
        } else if starts_ident(c) {
            lex_ident(src, i)?
        } else {
            lex_symbol(src, i)?
        };
        out.push((tok, i));
        i = next;
    }
    out.push((Tok::End, src.len()));
    Ok(out)
}

struct P<'a> {
    t: Vec<(Tok, usize)>,
    i: usize,
    syms: &'a HashMap<String, usize>,
}

/// (precedence, right-assoc, op) of a binary operator symbol.
fn binding(s: &str) -> Option<(u8, bool, BinFn)> {
    BINDINGS
        .iter()
        .find(|b| b.0 == s)
        .map(|&(_, prec, right, f)| (prec, right, f))
}

impl P<'_> {
    fn peek(&self) -> &Tok {
        &self.t[self.i].0
    }
    fn pos(&self) -> usize {
        self.t[self.i].1
    }
    fn err<T>(&self, msg: impl Into<String>) -> Result<T, ParseError> {
        Err(err_at(self.pos(), msg))
    }
    fn eat(&mut self, s: &str) -> bool {
        if matches!(self.peek(), Tok::Sym(x) if *x == s) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, s: &str) -> Result<(), ParseError> {
        if self.eat(s) {
            Ok(())
        } else {
            self.err(format!("expected `{s}`"))
        }
    }

    fn ternary(&mut self) -> Result<Node, ParseError> {
        let c = self.bin(0)?;
        if self.eat("?") {
            let a = self.ternary()?;
            self.expect(":")?;
            let b = self.ternary()?;
            return Ok(Node::Tern(c.into(), a.into(), b.into()));
        }
        Ok(c)
    }

    fn bin(&mut self, min: u8) -> Result<Node, ParseError> {
        let mut lhs = self.unary()?;
        while let Tok::Sym(s) = self.peek().clone() {
            let Some((prec, right, op)) = binding(s) else {
                break;
            };
            if prec < min {
                break;
            }
            self.i += 1;
            let rhs = self.bin(if right { prec } else { prec + 1 })?;
            lhs = Node::Bin(op, lhs.into(), rhs.into());
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Node, ParseError> {
        // unary binds looser than `^` (-2^2 = -4) but tighter than `*`
        if self.eat("-") {
            return Ok(Node::Neg(self.bin(8)?.into()));
        }
        if self.eat("!") {
            return Ok(Node::Not(self.bin(8)?.into()));
        }
        self.atom()
    }

    fn atom(&mut self) -> Result<Node, ParseError> {
        let pos = self.pos();
        match self.peek().clone() {
            Tok::Num(v) => {
                self.i += 1;
                Ok(Node::Num(v))
            }
            Tok::Sym("(") => {
                self.i += 1;
                let e = self.ternary()?;
                self.expect(")")?;
                Ok(e)
            }
            Tok::Ident(name) => {
                self.i += 1;
                self.ident(&name, pos)
            }
            _ => self.err("expected a value"),
        }
    }

    /// A name: a call when `(` follows, else a variable.
    fn ident(&mut self, name: &str, pos: usize) -> Result<Node, ParseError> {
        if *self.peek() == Tok::Sym("(") {
            self.i += 1;
            return self.call(name, pos);
        }
        match self.syms.get(name) {
            Some(&slot) => Ok(Node::Var(slot)),
            None => {
                let hint = did_you_mean(name, self.syms.keys().map(String::as_str));
                Err(err_at(pos, format!("unknown identifier `{name}`{hint}")))
            }
        }
    }

    /// Comma-separated arguments up to the closing `)`.
    fn args(&mut self) -> Result<Vec<Node>, ParseError> {
        let mut args = vec![];
        if self.eat(")") {
            return Ok(args);
        }
        loop {
            args.push(self.ternary()?);
            if self.eat(")") {
                return Ok(args);
            }
            self.expect(",")?;
        }
    }

    fn call(&mut self, name: &str, pos: usize) -> Result<Node, ParseError> {
        let args = self.args()?;
        if name == "if" {
            let [c, x, y]: [Node; 3] = args
                .try_into()
                .map_err(|_| err_at(pos, "if() takes 3 arguments"))?;
            return Ok(Node::Tern(c.into(), x.into(), y.into()));
        }
        let &(_, n, f) = FUNCS
            .iter()
            .find(|f| f.0 == name)
            .ok_or_else(|| err_at(pos, format!("unknown function `{name}`")))?;
        if args.len() != n {
            let got = args.len();
            return Err(err_at(
                pos,
                format!("{name}() takes {n} argument(s), got {got}"),
            ));
        }
        Ok(Node::Call(f, args))
    }
}

/// Parse `src`, resolving identifiers through `syms` (name -> slot).
pub fn compile(src: &str, syms: &HashMap<String, usize>) -> Result<Node, ParseError> {
    let mut p = P {
        t: lex(src)?,
        i: 0,
        syms,
    };
    let n = p.ternary()?;
    if *p.peek() != Tok::End {
        return p.err("unexpected trailing input");
    }
    Ok(n)
}

/// Argument values in call order; unused slots stay 0.
fn eval_args(a: &[Node], v: &[f64]) -> [f64; 3] {
    let mut xs = [0.0; 3];
    for (x, n) in xs.iter_mut().zip(a) {
        *x = n.eval(v);
    }
    xs
}

impl Node {
    pub fn eval(&self, v: &[f64]) -> f64 {
        match self {
            Node::Num(x) => *x,
            Node::Var(i) => v[*i],
            Node::Neg(a) => -a.eval(v),
            Node::Not(a) => b(a.eval(v) == 0.0),
            Node::Tern(c, x, y) => {
                if c.eval(v) != 0.0 {
                    x.eval(v)
                } else {
                    y.eval(v)
                }
            }
            Node::Bin(op, l, r) => op(l.eval(v), r.eval(v)),
            Node::Call(f, a) => f(&eval_args(a, v)),
        }
    }

    /// Slots this expression reads.
    pub fn vars(&self, out: &mut Vec<usize>) {
        match self {
            Node::Num(_) => {}
            Node::Var(i) => out.push(*i),
            Node::Neg(a) | Node::Not(a) => a.vars(out),
            Node::Bin(_, l, r) => {
                l.vars(out);
                r.vars(out);
            }
            Node::Tern(c, x, y) => {
                c.vars(out);
                x.vars(out);
                y.vars(out);
            }
            Node::Call(_, a) => a.iter().for_each(|n| n.vars(out)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(src: &str) -> f64 {
        let syms: HashMap<String, usize> = [
            ("a".to_string(), 0),
            ("bus-3.voltage".to_string(), 1),
            ("x.y".to_string(), 2),
        ]
        .into();
        compile(src, &syms).unwrap().eval(&[3.0, 0.95, 4.0])
    }

    #[test]
    fn precedence_and_ops() {
        assert_eq!(ev("1 + 2 * 3"), 7.0);
        assert_eq!(ev("(1 + 2) * 3"), 9.0);
        assert_eq!(ev("2 ^ 3 ^ 2"), 512.0);
        assert_eq!(ev("-2 ^ 2"), -4.0);
        assert_eq!(ev("10 % 4 - 1"), 1.0);
        assert_eq!(ev("1 < 2 && 2 <= 2 || 0"), 1.0);
        assert_eq!(ev("!0 + !5"), 1.0);
        assert_eq!(ev("a != 3"), 0.0);
        assert_eq!(ev("1e-3 * 1000"), 1.0);
    }

    #[test]
    fn ternary_if_and_funcs() {
        assert_eq!(ev("a > 2 ? 10 : 20"), 10.0);
        assert_eq!(ev("a > 5 ? 10 : a > 2 ? 20 : 30"), 20.0);
        assert_eq!(ev("if(a == 3, 1, 2)"), 1.0);
        assert_eq!(ev("max(a, 7) + min(1, 2) + abs(-1)"), 9.0);
        assert_eq!(
            ev("clamp(a, 0, 2) + sqrt(16) + sign(-9) + step(0) + floor(1.9) + pow(2, 3)"),
            2.0 + 4.0 - 1.0 + 1.0 + 1.0 + 8.0
        );
        assert!((ev("ln(exp(2))") - 2.0).abs() < 1e-12);
    }

    #[test]
    fn backquoted_tags() {
        assert_eq!(ev("`bus-3`.voltage > 0.9"), 1.0);
        assert_eq!(ev("`bus-3.voltage` * 2"), 1.9);
        assert_eq!(ev("x.y"), 4.0);
    }

    #[test]
    fn errors_carry_position() {
        let syms: HashMap<String, usize> = HashMap::new();
        let e = compile("1 + nope", &syms).unwrap_err();
        assert_eq!(e.pos, 4);
        assert!(e.msg.contains("`nope`"));
        assert!(compile("1 +", &syms).is_err());
        assert!(compile("(1", &syms).is_err());
        assert!(compile("min(1)", &syms)
            .unwrap_err()
            .msg
            .contains("2 argument"));
        assert!(compile("1 $ 2", &syms).unwrap_err().msg.contains('$'));
        assert!(compile("1 2", &syms).is_err());
    }

    #[test]
    fn syntax_errors_name_the_problem() {
        let syms: HashMap<String, usize> = HashMap::new();
        let msg = |s: &str| compile(s, &syms).unwrap_err().msg;
        assert!(msg("if(1,2)").contains("if() takes 3"));
        assert!(msg("frob(1)").contains("unknown function `frob`"));
        assert!(msg("`abc").contains("unterminated back-quote"));
        assert!(msg("1.2.3").contains("bad number `1.2.3`"));
        assert!(msg("min(1 2)").contains("expected `,`"));
        assert!(msg("2e").contains("unexpected trailing input")); // `e` without digits is not an exponent
        assert!(msg("").contains("expected a value"));
    }

    #[test]
    fn number_forms_and_edge_functions() {
        assert_eq!(ev("2.5e+1"), 25.0);
        assert_eq!(ev("2E2 + .5"), 200.5);
        assert_eq!(ev("5e-1"), 0.5);
        assert_eq!(
            ev("sign(0) + sign(3) * 10 + step(-1) + abs(-2) + exp(0)"),
            13.0
        );
        assert_eq!(ev("0 ? 1 : 2"), 2.0);
        assert_eq!(ev("if(0, 1, 2)"), 2.0);
        assert_eq!(ev("7 / 2 - 7 % 4 + 2 ^ 0.5 * 0"), 0.5);
        assert_eq!(ev("1 == 1 && 1 != 2 && 2 >= 2 && 3 > 2 && 1 < 2"), 1.0);
        assert_eq!(ev("0 || 0"), 0.0);
        assert_eq!(ev("clamp(-5, 0, 2) + clamp(9, 0, 2)"), 2.0);
        assert_eq!(ev("min(4, 2) + pow(2, 2) + floor(-0.5)"), 5.0);
    }

    #[test]
    fn vars_lists_every_slot_read() {
        let syms: HashMap<String, usize> = [("a".to_string(), 0), ("b".to_string(), 1)].into();
        let n = compile("a + max(b, 1) > 0 ? !a : -b", &syms).unwrap();
        let mut out = vec![];
        n.vars(&mut out);
        out.sort();
        assert_eq!(out, vec![0, 0, 1, 1]);
    }
}
