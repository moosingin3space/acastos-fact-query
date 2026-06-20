//! A textual frontend that mirrors Ascent's surface syntax, for fixtures,
//! tests, and human authoring. It lowers source text into the [`Program`] IR.
//!
//! # Grammar (informal)
//!
//! ```text
//! program    := (decl | rule)*
//! decl       := ("relation" | "lattice") ident "(" col ("," col)* ")" ";"
//! col        := "dual"? type            // "dual" only on a lattice's last col
//! type       := "int" | "bool" | "sym"
//! rule       := head ("," head)* "<--" clause ("," clause)* ";"
//! head       := ident "(" expr ("," expr)* ")"
//! clause     := atom | "!" atom | "if" expr | "let" ident "=" expr | agg
//! agg        := "agg" ident "=" aggfn "(" expr? ")" "in" atom
//! atom       := ident "(" arg ("," arg)* ")"
//! arg        := ident | "_" | int | "true" | "false" | string
//!             | "?" ("Dual" "(" ident ")" | ident)
//! ```

use crate::expr::{BinOp, Expr, UnOp};
use crate::ir::{
    AggFunc, Aggregate, Arg, Atom, BodyClause, HeadAtom, LatticeKind, Program, RelationDecl,
    RelationKind, Rule,
};
use crate::value::{Interner, Type, Value};

/// An error produced while parsing source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// A human-readable description of the problem.
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "parse error: {}", self.message)
    }
}

impl std::error::Error for ParseError {}

fn err<T>(message: impl Into<String>) -> Result<T, ParseError> {
    Err(ParseError {
        message: message.into(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Int(i64),
    Str(String),
    LParen,
    RParen,
    Comma,
    Semi,
    Arrow,
    Eq,
    EqEq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    AndAnd,
    OrOr,
    Bang,
    Question,
    Underscore,
}

fn tokenize(src: &str) -> Result<Vec<Tok>, ParseError> {
    let bytes = src.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if let Some(tok) = single_char(c) {
            out.push(tok);
            i += 1;
        } else {
            i = lex_complex(src, bytes, i, &mut out)?;
        }
    }
    Ok(out)
}

/// Lexes a one-byte token that is unambiguous on its own.
fn single_char(c: u8) -> Option<Tok> {
    Some(match c {
        b'(' => Tok::LParen,
        b')' => Tok::RParen,
        b',' => Tok::Comma,
        b';' => Tok::Semi,
        b'+' => Tok::Plus,
        b'*' => Tok::Star,
        b'/' => Tok::Slash,
        b'%' => Tok::Percent,
        b'?' => Tok::Question,
        _ => return None,
    })
}

/// Lexes a multi-byte operator, literal, or word; returns the next position.
fn lex_complex(src: &str, bytes: &[u8], i: usize, out: &mut Vec<Tok>) -> Result<usize, ParseError> {
    let next_is = |b: u8| bytes.get(i + 1) == Some(&b);
    match bytes[i] {
        b'-' if next_is(b'-') => err("unexpected `--`; did you mean `<--`?"),
        b'-' => Ok(push(out, Tok::Minus, i + 1)),
        b'<' if src[i..].starts_with("<--") => Ok(push(out, Tok::Arrow, i + 3)),
        b'<' if next_is(b'=') => Ok(push(out, Tok::Le, i + 2)),
        b'<' => Ok(push(out, Tok::Lt, i + 1)),
        b'>' if next_is(b'=') => Ok(push(out, Tok::Ge, i + 2)),
        b'>' => Ok(push(out, Tok::Gt, i + 1)),
        b'=' if next_is(b'=') => Ok(push(out, Tok::EqEq, i + 2)),
        b'=' => Ok(push(out, Tok::Eq, i + 1)),
        b'!' if next_is(b'=') => Ok(push(out, Tok::Ne, i + 2)),
        b'!' => Ok(push(out, Tok::Bang, i + 1)),
        b'&' if next_is(b'&') => Ok(push(out, Tok::AndAnd, i + 2)),
        b'|' if next_is(b'|') => Ok(push(out, Tok::OrOr, i + 2)),
        b'"' => lex_string(src, bytes, i, out),
        b'0'..=b'9' => lex_number(src, bytes, i, out),
        c if c == b'_' || c.is_ascii_alphabetic() => Ok(lex_word(src, bytes, i, out)),
        other => err(format!("unexpected character `{}`", other as char)),
    }
}

fn push(out: &mut Vec<Tok>, tok: Tok, next: usize) -> usize {
    out.push(tok);
    next
}

fn lex_string(src: &str, bytes: &[u8], i: usize, out: &mut Vec<Tok>) -> Result<usize, ParseError> {
    let start = i + 1;
    let mut j = start;
    while j < bytes.len() && bytes[j] != b'"' {
        j += 1;
    }
    if j >= bytes.len() {
        return err("unterminated string literal");
    }
    out.push(Tok::Str(src[start..j].to_owned()));
    Ok(j + 1)
}

fn lex_number(src: &str, bytes: &[u8], i: usize, out: &mut Vec<Tok>) -> Result<usize, ParseError> {
    let mut j = i;
    while j < bytes.len() && bytes[j].is_ascii_digit() {
        j += 1;
    }
    let text = &src[i..j];
    let n: i64 = text.parse().map_err(|_| ParseError {
        message: format!("bad integer `{text}`"),
    })?;
    out.push(Tok::Int(n));
    Ok(j)
}

fn lex_word(src: &str, bytes: &[u8], i: usize, out: &mut Vec<Tok>) -> usize {
    let mut j = i;
    while j < bytes.len() {
        let c = bytes[j];
        if c == b'_' || c.is_ascii_alphanumeric() {
            j += 1;
        } else if c == b':' && bytes.get(j + 1) == Some(&b':') {
            // A `::` path separator inside a name, so namespaced relations like
            // `policy::grant` / `policy::deny` read naturally. A lone `:` is
            // still rejected by the lexer.
            j += 2;
        } else {
            break;
        }
    }
    let text = &src[i..j];
    if text == "_" {
        out.push(Tok::Underscore);
    } else {
        out.push(Tok::Ident(text.to_owned()));
    }
    j
}

/// Parses `src` into a [`Program`], interning symbols into `interner`.
///
/// # Errors
///
/// Returns [`ParseError`] for any lexical or grammatical problem.
pub fn parse(src: &str, interner: &mut Interner) -> Result<Program, ParseError> {
    let toks = tokenize(src)?;
    let mut p = Parser {
        toks,
        pos: 0,
        interner,
    };
    p.program()
}

/// Parses a conjunctive query — `"(" expr ("," expr)* ")" "<--" clause ("," clause)*
/// ";"?` — into its output expressions and body clauses, interning symbols into
/// `interner`. This is the queries-grain front end: a rule body with an
/// explicit, head-relation-free output tuple. It reuses the rule-body grammar
/// verbatim, so joins, `if` filters, `let` bindings, and aggregates are all
/// accepted here; restricting the body to the conjunctive fragment (rejecting
/// negation) and checking range-restriction are the caller's form checks, not
/// the parser's.
///
/// # Errors
///
/// Returns [`ParseError`] for any lexical or grammatical problem, or trailing
/// tokens after the query.
pub fn parse_query(
    src: &str,
    interner: &mut Interner,
) -> Result<(Vec<Expr>, Vec<BodyClause>), ParseError> {
    let toks = tokenize(src)?;
    let mut p = Parser {
        toks,
        pos: 0,
        interner,
    };
    let query = p.query()?;
    if p.peek().is_some() {
        return err(format!("trailing tokens after query: {:?}", p.peek()));
    }
    Ok(query)
}

struct Parser<'a> {
    toks: Vec<Tok>,
    pos: usize,
    interner: &'a mut Interner,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Result<Tok, ParseError> {
        let t = self.toks.get(self.pos).cloned().ok_or_else(|| ParseError {
            message: "unexpected end of input".to_owned(),
        })?;
        self.pos += 1;
        Ok(t)
    }

    fn eat(&mut self, t: &Tok) -> Result<(), ParseError> {
        let got = self.next()?;
        if &got == t {
            Ok(())
        } else {
            err(format!("expected {t:?}, found {got:?}"))
        }
    }

    fn ident(&mut self) -> Result<String, ParseError> {
        match self.next()? {
            Tok::Ident(s) => Ok(s),
            other => err(format!("expected identifier, found {other:?}")),
        }
    }

    fn program(&mut self) -> Result<Program, ParseError> {
        let mut prog = Program::default();
        while self.peek().is_some() {
            if matches!(self.peek(), Some(Tok::Ident(k)) if k == "relation" || k == "lattice") {
                prog.relations.push(self.decl()?);
            } else {
                prog.rules.push(self.rule()?);
            }
        }
        Ok(prog)
    }

    fn decl(&mut self) -> Result<RelationDecl, ParseError> {
        let kw = self.ident()?;
        let is_lattice = kw == "lattice";
        let name = self.ident()?;
        self.eat(&Tok::LParen)?;
        let mut schema = Vec::new();
        let mut duals = Vec::new();
        loop {
            let (ty, dual) = self.column()?;
            schema.push(ty);
            duals.push(dual);
            match self.next()? {
                Tok::Comma => {}
                Tok::RParen => break,
                other => return err(format!("expected `,` or `)`, found {other:?}")),
            }
        }
        self.eat(&Tok::Semi)?;
        let kind = if is_lattice {
            if schema.is_empty() {
                return err("lattice needs at least one column");
            }
            for (idx, dual) in duals.iter().enumerate() {
                if *dual && idx + 1 != schema.len() {
                    return err("only a lattice's last column may be `dual`");
                }
            }
            let lk = if duals[schema.len() - 1] {
                LatticeKind::Min
            } else {
                LatticeKind::Max
            };
            RelationKind::Lattice(lk)
        } else {
            if duals.iter().any(|d| *d) {
                return err("`dual` is only valid in a `lattice` declaration");
            }
            RelationKind::Relation
        };
        Ok(RelationDecl { name, schema, kind })
    }

    fn column(&mut self) -> Result<(Type, bool), ParseError> {
        let mut dual = false;
        let mut name = self.ident()?;
        if name == "dual" {
            dual = true;
            name = self.ident()?;
        }
        let ty = match name.as_str() {
            "int" => Type::Int,
            "bool" => Type::Bool,
            "sym" => Type::Sym,
            other => return err(format!("unknown type `{other}`")),
        };
        Ok((ty, dual))
    }

    fn rule(&mut self) -> Result<Rule, ParseError> {
        let mut heads = vec![self.head_atom()?];
        while matches!(self.peek(), Some(Tok::Comma)) {
            self.eat(&Tok::Comma)?;
            heads.push(self.head_atom()?);
        }
        self.eat(&Tok::Arrow)?;
        let mut body = vec![self.clause()?];
        while matches!(self.peek(), Some(Tok::Comma)) {
            self.eat(&Tok::Comma)?;
            body.push(self.clause()?);
        }
        self.eat(&Tok::Semi)?;
        Ok(Rule { heads, body })
    }

    /// Parses a conjunctive query body: `( expr, .. ) <-- clause, ..` with an
    /// optional trailing `;`. The output tuple must be non-empty.
    fn query(&mut self) -> Result<(Vec<Expr>, Vec<BodyClause>), ParseError> {
        self.eat(&Tok::LParen)?;
        if matches!(self.peek(), Some(Tok::RParen)) {
            return err("a query must have at least one output expression");
        }
        let mut outputs = Vec::new();
        loop {
            outputs.push(self.expr()?);
            match self.next()? {
                Tok::Comma => {}
                Tok::RParen => break,
                other => return err(format!("expected `,` or `)`, found {other:?}")),
            }
        }
        self.eat(&Tok::Arrow)?;
        let mut body = vec![self.clause()?];
        while matches!(self.peek(), Some(Tok::Comma)) {
            self.eat(&Tok::Comma)?;
            body.push(self.clause()?);
        }
        if matches!(self.peek(), Some(Tok::Semi)) {
            self.eat(&Tok::Semi)?;
        }
        Ok((outputs, body))
    }

    fn head_atom(&mut self) -> Result<HeadAtom, ParseError> {
        let relation = self.ident()?;
        self.eat(&Tok::LParen)?;
        let mut args = Vec::new();
        if matches!(self.peek(), Some(Tok::RParen)) {
            self.eat(&Tok::RParen)?;
        } else {
            loop {
                args.push(self.expr()?);
                match self.next()? {
                    Tok::Comma => {}
                    Tok::RParen => break,
                    other => return err(format!("expected `,` or `)`, found {other:?}")),
                }
            }
        }
        Ok(HeadAtom { relation, args })
    }

    fn clause(&mut self) -> Result<BodyClause, ParseError> {
        match self.peek() {
            Some(Tok::Bang) => {
                self.eat(&Tok::Bang)?;
                Ok(BodyClause::Negative(self.atom()?))
            }
            Some(Tok::Ident(k)) if k == "if" => {
                self.next()?;
                Ok(BodyClause::Condition(self.expr()?))
            }
            Some(Tok::Ident(k)) if k == "let" => {
                self.next()?;
                let var = self.ident()?;
                let var = self.interner.intern(&var);
                self.eat(&Tok::Eq)?;
                let expr = self.expr()?;
                Ok(BodyClause::Let { var, expr })
            }
            Some(Tok::Ident(k)) if k == "agg" => self.aggregate(),
            _ => Ok(BodyClause::Positive(self.atom()?)),
        }
    }

    fn aggregate(&mut self) -> Result<BodyClause, ParseError> {
        self.next()?; // `agg`
        let out = self.ident()?;
        let output = self.interner.intern(&out);
        self.eat(&Tok::Eq)?;
        let func = match self.ident()?.as_str() {
            "count" => AggFunc::Count,
            "sum" => AggFunc::Sum,
            "min" => AggFunc::Min,
            "max" => AggFunc::Max,
            other => return err(format!("unknown aggregator `{other}`")),
        };
        self.eat(&Tok::LParen)?;
        let arg = if matches!(self.peek(), Some(Tok::RParen)) {
            None
        } else {
            Some(self.expr()?)
        };
        self.eat(&Tok::RParen)?;
        let in_kw = self.ident()?;
        if in_kw != "in" {
            return err("expected `in` after aggregator");
        }
        let source = self.atom()?;
        Ok(BodyClause::Aggregate(Aggregate {
            output,
            func,
            arg,
            source,
        }))
    }

    fn atom(&mut self) -> Result<Atom, ParseError> {
        let relation = self.ident()?;
        self.eat(&Tok::LParen)?;
        let mut args = Vec::new();
        if matches!(self.peek(), Some(Tok::RParen)) {
            self.eat(&Tok::RParen)?;
        } else {
            loop {
                args.push(self.arg()?);
                match self.next()? {
                    Tok::Comma => {}
                    Tok::RParen => break,
                    other => return err(format!("expected `,` or `)`, found {other:?}")),
                }
            }
        }
        Ok(Atom { relation, args })
    }

    fn arg(&mut self) -> Result<Arg, ParseError> {
        match self.next()? {
            Tok::Underscore => Ok(Arg::Wildcard),
            Tok::Int(n) => Ok(Arg::Lit(Value::Int(n))),
            Tok::Minus => match self.next()? {
                Tok::Int(n) => Ok(Arg::Lit(Value::Int(-n))),
                other => err(format!("expected integer after `-`, found {other:?}")),
            },
            Tok::Str(s) => {
                let sym = self.interner.intern(&s);
                Ok(Arg::Lit(Value::Sym(sym)))
            }
            Tok::Question => {
                // `?Dual(x)` or `?x`: read a lattice value column.
                if matches!(self.peek(), Some(Tok::Ident(k)) if k == "Dual") {
                    self.next()?;
                    self.eat(&Tok::LParen)?;
                    let v = self.ident()?;
                    self.eat(&Tok::RParen)?;
                    Ok(Arg::LatticeBind(self.interner.intern(&v)))
                } else {
                    let v = self.ident()?;
                    Ok(Arg::LatticeBind(self.interner.intern(&v)))
                }
            }
            Tok::Ident(s) => match s.as_str() {
                "true" => Ok(Arg::Lit(Value::Bool(true))),
                "false" => Ok(Arg::Lit(Value::Bool(false))),
                _ => Ok(Arg::Var(self.interner.intern(&s))),
            },
            other => err(format!("unexpected token in argument: {other:?}")),
        }
    }

    // ----- expressions -----

    fn expr(&mut self) -> Result<Expr, ParseError> {
        self.expr_or()
    }

    fn expr_or(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.expr_and()?;
        while matches!(self.peek(), Some(Tok::OrOr)) {
            self.next()?;
            let rhs = self.expr_and()?;
            lhs = Expr::Binary(BinOp::Or, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn expr_and(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.expr_cmp()?;
        while matches!(self.peek(), Some(Tok::AndAnd)) {
            self.next()?;
            let rhs = self.expr_cmp()?;
            lhs = Expr::Binary(BinOp::And, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn expr_cmp(&mut self) -> Result<Expr, ParseError> {
        let lhs = self.expr_add()?;
        let op = match self.peek() {
            Some(Tok::EqEq) => BinOp::Eq,
            Some(Tok::Ne) => BinOp::Ne,
            Some(Tok::Lt) => BinOp::Lt,
            Some(Tok::Le) => BinOp::Le,
            Some(Tok::Gt) => BinOp::Gt,
            Some(Tok::Ge) => BinOp::Ge,
            _ => return Ok(lhs),
        };
        self.next()?;
        let rhs = self.expr_add()?;
        Ok(Expr::Binary(op, Box::new(lhs), Box::new(rhs)))
    }

    fn expr_add(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.expr_mul()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => BinOp::Add,
                Some(Tok::Minus) => BinOp::Sub,
                _ => break,
            };
            self.next()?;
            let rhs = self.expr_mul()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn expr_mul(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.expr_cast()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Star) => BinOp::Mul,
                Some(Tok::Slash) => BinOp::Div,
                Some(Tok::Percent) => BinOp::Rem,
                _ => break,
            };
            self.next()?;
            let rhs = self.expr_cast()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn expr_cast(&mut self) -> Result<Expr, ParseError> {
        let inner = self.expr_unary()?;
        // `expr as type` is accepted and ignored: our value model is untyped
        // across integer widths, so casts between integer types are identities.
        if matches!(self.peek(), Some(Tok::Ident(k)) if k == "as") {
            self.next()?;
            let _ty = self.ident()?;
            return Ok(inner);
        }
        Ok(inner)
    }

    fn expr_unary(&mut self) -> Result<Expr, ParseError> {
        match self.peek() {
            Some(Tok::Minus) => {
                self.next()?;
                Ok(Expr::Unary(UnOp::Neg, Box::new(self.expr_unary()?)))
            }
            Some(Tok::Bang) => {
                self.next()?;
                Ok(Expr::Unary(UnOp::Not, Box::new(self.expr_unary()?)))
            }
            _ => self.expr_primary(),
        }
    }

    fn expr_primary(&mut self) -> Result<Expr, ParseError> {
        match self.next()? {
            Tok::LParen => {
                let e = self.expr()?;
                self.eat(&Tok::RParen)?;
                Ok(e)
            }
            Tok::Int(n) => Ok(Expr::Lit(Value::Int(n))),
            Tok::Str(s) => Ok(Expr::Lit(Value::Sym(self.interner.intern(&s)))),
            Tok::Ident(s) => match s.as_str() {
                "true" => Ok(Expr::Lit(Value::Bool(true))),
                "false" => Ok(Expr::Lit(Value::Bool(false))),
                // `Dual(expr)` in a head is a lattice constructor; the relation
                // declaration already records the ordering, so we unwrap it.
                "Dual" => {
                    self.eat(&Tok::LParen)?;
                    let e = self.expr()?;
                    self.eat(&Tok::RParen)?;
                    Ok(e)
                }
                _ => Ok(Expr::Var(self.interner.intern(&s))),
            },
            other => err(format!("unexpected token in expression: {other:?}")),
        }
    }
}
