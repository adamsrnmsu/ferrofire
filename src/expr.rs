//! Tiny expression language for model files (spec section 3.1). Pratt parser -> tree, resolved to value slots at
//! compile time; evaluation is allocation-free.
//!
//! numbers, identifiers `[A-Za-z_][A-Za-z0-9_.]*` or back-quoted tags (`` `bus-3`.voltage ``), `+ - * / % ^`,
//! unary `- !`, `< <= > >= == !=` (1/0), `&& ||`, `c ? a : b`, `if(c,a,b)`,
//! functions `min max abs sqrt exp ln pow clamp sign step floor`.

use std::collections::HashMap;

#[derive(Clone, Debug)]
pub enum Node {
    Num(f64),
    Var(usize),
    Neg(Box<Node>),
    Not(Box<Node>),
    Bin(Op, Box<Node>, Box<Node>),
    Tern(Box<Node>, Box<Node>, Box<Node>),
    Call(Func, Vec<Node>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op { Add, Sub, Mul, Div, Rem, Pow, Lt, Le, Gt, Ge, Eq, Ne, And, Or }

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Func { Min, Max, Abs, Sqrt, Exp, Ln, Pow, Clamp, Sign, Step, Floor }

impl Func {
    fn lookup(name: &str) -> Option<(Func, usize)> {
        Some(match name {
            "min" => (Func::Min, 2), "max" => (Func::Max, 2), "abs" => (Func::Abs, 1), "sqrt" => (Func::Sqrt, 1),
            "exp" => (Func::Exp, 1), "ln" => (Func::Ln, 1), "pow" => (Func::Pow, 2), "clamp" => (Func::Clamp, 3),
            "sign" => (Func::Sign, 1), "step" => (Func::Step, 1), "floor" => (Func::Floor, 1),
            _ => return None,
        })
    }
}

#[derive(Debug)]
pub struct ParseError { pub pos: usize, pub msg: String }
impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { write!(f, "at column {}: {}", self.pos + 1, self.msg) }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok { Num(f64), Ident(String), Sym(&'static str), End }

const SYMS: [&str; 20] = ["<=", ">=", "==", "!=", "&&", "||", "+", "-", "*", "/", "%", "^", "<", ">", "!", "?", ":", "(", ")", ","];

fn lex(src: &str) -> Result<Vec<(Tok, usize)>, ParseError> {
    let b = src.as_bytes();
    let (mut i, mut out) = (0, vec![]);
    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_whitespace() { i += 1; continue; }
        let start = i;
        if c.is_ascii_digit() || (c == '.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') { i += 1; }
            if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                let mut j = i + 1;
                if j < b.len() && (b[j] == b'+' || b[j] == b'-') { j += 1; }
                if j < b.len() && b[j].is_ascii_digit() {
                    while j < b.len() && b[j].is_ascii_digit() { j += 1; }
                    i = j;
                }
            }
            let v = src[start..i].parse::<f64>().map_err(|_| ParseError { pos: start, msg: format!("bad number `{}`", &src[start..i]) })?;
            out.push((Tok::Num(v), start));
        } else if c.is_ascii_alphabetic() || c == '_' || c == '`' {
            // identifier: runs of [A-Za-z0-9_.] and back-quoted segments (tags containing `-`)
            let mut name = String::new();
            loop {
                if i < b.len() && b[i] == b'`' {
                    let end = src[i + 1..].find('`').ok_or(ParseError { pos: i, msg: "unterminated back-quote".into() })?;
                    name.push_str(&src[i + 1..i + 1 + end]);
                    i += end + 2;
                } else if i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.') {
                    name.push(b[i] as char);
                    i += 1;
                } else { break; }
            }
            out.push((Tok::Ident(name), start));
        } else {
            let s = SYMS.iter().find(|s| src[i..].starts_with(**s)).ok_or(ParseError { pos: i, msg: format!("unexpected `{c}`") })?;
            i += s.len();
            out.push((Tok::Sym(s), start));
        }
    }
    out.push((Tok::End, src.len()));
    Ok(out)
}

struct P<'a> { t: Vec<(Tok, usize)>, i: usize, syms: &'a HashMap<String, usize> }

fn binding(s: &str) -> Option<(u8, bool, Op)> { // (precedence, right-assoc, op)
    Some(match s {
        "||" => (2, false, Op::Or), "&&" => (3, false, Op::And),
        "==" => (4, false, Op::Eq), "!=" => (4, false, Op::Ne),
        "<" => (5, false, Op::Lt), "<=" => (5, false, Op::Le), ">" => (5, false, Op::Gt), ">=" => (5, false, Op::Ge),
        "+" => (6, false, Op::Add), "-" => (6, false, Op::Sub),
        "*" => (7, false, Op::Mul), "/" => (7, false, Op::Div), "%" => (7, false, Op::Rem),
        "^" => (9, true, Op::Pow),
        _ => return None,
    })
}

impl P<'_> {
    fn peek(&self) -> &Tok { &self.t[self.i].0 }
    fn pos(&self) -> usize { self.t[self.i].1 }
    fn err<T>(&self, msg: impl Into<String>) -> Result<T, ParseError> { Err(ParseError { pos: self.pos(), msg: msg.into() }) }
    fn eat(&mut self, s: &str) -> bool { if matches!(self.peek(), Tok::Sym(x) if *x == s) { self.i += 1; true } else { false } }
    fn expect(&mut self, s: &str) -> Result<(), ParseError> { if self.eat(s) { Ok(()) } else { self.err(format!("expected `{s}`")) } }

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
            let Some((prec, right, op)) = binding(s) else { break };
            if prec < min { break; }
            self.i += 1;
            let rhs = self.bin(if right { prec } else { prec + 1 })?;
            lhs = Node::Bin(op, lhs.into(), rhs.into());
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Node, ParseError> {
        // unary binds looser than `^` (-2^2 = -4) but tighter than `*`
        if self.eat("-") { return Ok(Node::Neg(self.bin(8)?.into())); }
        if self.eat("!") { return Ok(Node::Not(self.bin(8)?.into())); }
        self.atom()
    }

    fn atom(&mut self) -> Result<Node, ParseError> {
        let pos = self.pos();
        match self.peek().clone() {
            Tok::Num(v) => { self.i += 1; Ok(Node::Num(v)) }
            Tok::Sym("(") => { self.i += 1; let e = self.ternary()?; self.expect(")")?; Ok(e) }
            Tok::Ident(name) => {
                self.i += 1;
                if *self.peek() == Tok::Sym("(") {
                    self.i += 1;
                    let mut args = vec![];
                    if !self.eat(")") {
                        loop {
                            args.push(self.ternary()?);
                            if self.eat(")") { break; }
                            self.expect(",")?;
                        }
                    }
                    if name == "if" {
                        if args.len() != 3 { return Err(ParseError { pos, msg: "if() takes 3 arguments".into() }); }
                        let mut it = args.into_iter();
                        return Ok(Node::Tern(it.next().unwrap().into(), it.next().unwrap().into(), it.next().unwrap().into()));
                    }
                    let (f, n) = Func::lookup(&name).ok_or(ParseError { pos, msg: format!("unknown function `{name}`") })?;
                    if args.len() != n { return Err(ParseError { pos, msg: format!("{name}() takes {n} argument(s), got {}", args.len()) }); }
                    return Ok(Node::Call(f, args));
                }
                match self.syms.get(&name) {
                    Some(&slot) => Ok(Node::Var(slot)),
                    None => Err(ParseError { pos, msg: format!("unknown identifier `{name}`") }),
                }
            }
            _ => self.err("expected a value"),
        }
    }
}

/// Parse `src`, resolving identifiers through `syms` (name -> slot).
pub fn compile(src: &str, syms: &HashMap<String, usize>) -> Result<Node, ParseError> {
    let mut p = P { t: lex(src)?, i: 0, syms };
    let n = p.ternary()?;
    if *p.peek() != Tok::End { return p.err("unexpected trailing input"); }
    Ok(n)
}

fn b(x: bool) -> f64 { x as u8 as f64 }

impl Node {
    pub fn eval(&self, v: &[f64]) -> f64 {
        match self {
            Node::Num(x) => *x,
            Node::Var(i) => v[*i],
            Node::Neg(a) => -a.eval(v),
            Node::Not(a) => b(a.eval(v) == 0.0),
            Node::Tern(c, x, y) => if c.eval(v) != 0.0 { x.eval(v) } else { y.eval(v) },
            Node::Bin(op, l, r) => {
                let (x, y) = (l.eval(v), r.eval(v));
                match op {
                    Op::Add => x + y, Op::Sub => x - y, Op::Mul => x * y, Op::Div => x / y, Op::Rem => x % y, Op::Pow => x.powf(y),
                    Op::Lt => b(x < y), Op::Le => b(x <= y), Op::Gt => b(x > y), Op::Ge => b(x >= y),
                    Op::Eq => b(x == y), Op::Ne => b(x != y),
                    Op::And => b(x != 0.0 && y != 0.0), Op::Or => b(x != 0.0 || y != 0.0),
                }
            }
            Node::Call(f, a) => {
                let x = a[0].eval(v);
                match f {
                    Func::Abs => x.abs(), Func::Sqrt => x.sqrt(), Func::Exp => x.exp(), Func::Ln => x.ln(),
                    Func::Sign => if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { 0.0 },
                    Func::Step => b(x >= 0.0), Func::Floor => x.floor(),
                    Func::Min => x.min(a[1].eval(v)), Func::Max => x.max(a[1].eval(v)), Func::Pow => x.powf(a[1].eval(v)),
                    Func::Clamp => x.max(a[1].eval(v)).min(a[2].eval(v)), // clamp(x, lo, hi)
                }
            }
        }
    }

    /// Slots this expression reads.
    pub fn vars(&self, out: &mut Vec<usize>) {
        match self {
            Node::Num(_) => {}
            Node::Var(i) => out.push(*i),
            Node::Neg(a) | Node::Not(a) => a.vars(out),
            Node::Bin(_, l, r) => { l.vars(out); r.vars(out); }
            Node::Tern(c, x, y) => { c.vars(out); x.vars(out); y.vars(out); }
            Node::Call(_, a) => a.iter().for_each(|n| n.vars(out)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(src: &str) -> f64 {
        let syms: HashMap<String, usize> = [("a".to_string(), 0), ("bus-3.voltage".to_string(), 1), ("x.y".to_string(), 2)].into();
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
        assert_eq!(ev("clamp(a, 0, 2) + sqrt(16) + sign(-9) + step(0) + floor(1.9) + pow(2, 3)"), 2.0 + 4.0 - 1.0 + 1.0 + 1.0 + 8.0);
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
        assert!(compile("min(1)", &syms).unwrap_err().msg.contains("2 argument"));
        assert!(compile("1 $ 2", &syms).unwrap_err().msg.contains('$'));
        assert!(compile("1 2", &syms).is_err());
    }
}
