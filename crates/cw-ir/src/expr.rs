//! Expression parser and AST for `derive` and `assert` expressions in `cw-ir`.

use cw_syntax::{ClaimRef, Diagnostic, DiagnosticCode, FamilyRef};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Expr {
    Number(f64),
    Bool(bool),
    Ref(ClaimRef),
    /// Shorthand `@epoch.field` within a family context
    EpochField {
        epoch: String,
        field: String,
    },
    UnaryNeg(Box<Expr>),
    Not(Box<Expr>),
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Reduction {
        op: ReductionOp,
        target: FamilyRef,
    },
    PrefixSum {
        epoch: Option<String>,
        field: String,
    },
    Round {
        expr: Box<Expr>,
        decimals: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReductionOp {
    Sum,
    Min,
    Max,
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    LParen,
    RParen,
    Comma,
    Plus,
    Minus,
    Star,
    Slash,
    EqEq,
    BangEq,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Not,
    IdentOrRef(String),
}

pub fn parse_expr(input: &str) -> Result<Expr, Diagnostic> {
    let tokens = tokenize_expr(input)?;
    if tokens.is_empty() {
        return Err(Diagnostic::error(
            DiagnosticCode::CW1001,
            "Empty expression",
        ));
    }
    let mut parser = ExprParser { tokens, pos: 0 };
    let expr = parser.parse_or()?;
    if parser.pos < parser.tokens.len() {
        return Err(Diagnostic::error(
            DiagnosticCode::CW1001,
            format!(
                "Unexpected trailing token `{:?}` in expression `{input}`",
                parser.tokens[parser.pos]
            ),
        ));
    }
    Ok(expr)
}

fn tokenize_expr(input: &str) -> Result<Vec<Token>, Diagnostic> {
    let mut tokens = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match bytes[i] {
            b'(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            b')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            b',' => {
                tokens.push(Token::Comma);
                i += 1;
            }
            b'+' => {
                tokens.push(Token::Plus);
                i += 1;
            }
            b'-' => {
                tokens.push(Token::Minus);
                i += 1;
            }
            b'*' => {
                tokens.push(Token::Star);
                i += 1;
            }
            b'/' => {
                tokens.push(Token::Slash);
                i += 1;
            }
            b'=' if i + 1 < bytes.len() && bytes[i + 1] == b'=' => {
                tokens.push(Token::EqEq);
                i += 2;
            }
            b'!' if i + 1 < bytes.len() && bytes[i + 1] == b'=' => {
                tokens.push(Token::BangEq);
                i += 2;
            }
            b'<' if i + 1 < bytes.len() && bytes[i + 1] == b'=' => {
                tokens.push(Token::Le);
                i += 2;
            }
            b'<' => {
                tokens.push(Token::Lt);
                i += 1;
            }
            b'>' if i + 1 < bytes.len() && bytes[i + 1] == b'=' => {
                tokens.push(Token::Ge);
                i += 2;
            }
            b'>' => {
                tokens.push(Token::Gt);
                i += 1;
            }
            b if b.is_ascii_digit() => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                    i += 1;
                }
                if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
                    i += 1;
                    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                        i += 1;
                    }
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let num_str = &input[start..i];
                let val = num_str.parse::<f64>().map_err(|_| {
                    Diagnostic::error(
                        DiagnosticCode::CW1001,
                        format!("Invalid numeric literal `{num_str}` in expression `{input}`"),
                    )
                })?;
                tokens.push(Token::Number(val));
            }
            b if b.is_ascii_alphabetic() || b == b'_' || b == b'@' => {
                let start = i;
                let mut bracket_depth = 0usize;
                let mut in_quotes = false;
                while i < bytes.len() {
                    let c = bytes[i];
                    if in_quotes {
                        if c == b'\\' {
                            i += 2;
                            continue;
                        }
                        if c == b'"' {
                            in_quotes = false;
                        }
                        i += 1;
                    } else if bracket_depth > 0 {
                        if c == b'"' {
                            in_quotes = true;
                        } else if c == b'[' {
                            bracket_depth += 1;
                        } else if c == b']' {
                            bracket_depth -= 1;
                        }
                        i += 1;
                    } else {
                        if c == b'[' {
                            bracket_depth += 1;
                            i += 1;
                        } else if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'@' {
                            i += 1;
                        } else {
                            break;
                        }
                    }
                }
                let raw = &input[start..i];
                match raw {
                    "and" => tokens.push(Token::And),
                    "or" => tokens.push(Token::Or),
                    "not" => tokens.push(Token::Not),
                    _ => tokens.push(Token::IdentOrRef(raw.to_string())),
                }
            }
            other => {
                return Err(Diagnostic::error(
                    DiagnosticCode::CW1001,
                    format!(
                        "Unexpected character `{}` in expression `{input}`",
                        other as char
                    ),
                ));
            }
        }
    }

    Ok(tokens)
}

struct ExprParser {
    tokens: Vec<Token>,
    pos: usize,
}

impl ExprParser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        if self.pos < self.tokens.len() {
            let t = self.tokens[self.pos].clone();
            self.pos += 1;
            Some(t)
        } else {
            None
        }
    }

    fn parse_or(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_and()?;
        while matches!(self.peek(), Some(Token::Or)) {
            self.next();
            let rhs = self.parse_and()?;
            lhs = Expr::Binary {
                op: BinOp::Or,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_cmp()?;
        while matches!(self.peek(), Some(Token::And)) {
            self.next();
            let rhs = self.parse_cmp()?;
            lhs = Expr::Binary {
                op: BinOp::And,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_cmp(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_add()?;
        while let Some(tok) = self.peek() {
            let op = match tok {
                Token::EqEq => BinOp::Eq,
                Token::BangEq => BinOp::Ne,
                Token::Lt => BinOp::Lt,
                Token::Le => BinOp::Le,
                Token::Gt => BinOp::Gt,
                Token::Ge => BinOp::Ge,
                _ => break,
            };
            self.next();
            let rhs = self.parse_add()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_add(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_mul()?;
        while let Some(tok) = self.peek() {
            let op = match tok {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.next();
            let rhs = self.parse_mul()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_mul(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_unary()?;
        while let Some(tok) = self.peek() {
            let op = match tok {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                _ => break,
            };
            self.next();
            let rhs = self.parse_unary()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, Diagnostic> {
        match self.peek() {
            Some(Token::Minus) => {
                self.next();
                let inner = self.parse_unary()?;
                Ok(Expr::UnaryNeg(Box::new(inner)))
            }
            Some(Token::Not) => {
                self.next();
                let inner = self.parse_unary()?;
                Ok(Expr::Not(Box::new(inner)))
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, Diagnostic> {
        let Some(tok) = self.next() else {
            return Err(Diagnostic::error(
                DiagnosticCode::CW1001,
                "Unexpected end of expression",
            ));
        };

        match tok {
            Token::Number(n) => Ok(Expr::Number(n)),
            Token::LParen => {
                let e = self.parse_or()?;
                if !matches!(self.next(), Some(Token::RParen)) {
                    return Err(Diagnostic::error(
                        DiagnosticCode::CW1001,
                        "Expected closing `)` in expression",
                    ));
                }
                Ok(e)
            }
            Token::IdentOrRef(ident) => {
                if ident == "true" {
                    return Ok(Expr::Bool(true));
                }
                if ident == "false" {
                    return Ok(Expr::Bool(false));
                }
                // Check function calls: `sum(...)`, `min(...)`, `max(...)`, `prefix_sum(...)`, `round(...)`
                if matches!(self.peek(), Some(Token::LParen)) {
                    self.next(); // consume `(`
                    match ident.as_str() {
                        "sum" | "min" | "max" => {
                            let op = match ident.as_str() {
                                "sum" => ReductionOp::Sum,
                                "min" => ReductionOp::Min,
                                "max" => ReductionOp::Max,
                                _ => unreachable!(),
                            };
                            let Some(Token::IdentOrRef(arg)) = self.next() else {
                                return Err(Diagnostic::error(
                                    DiagnosticCode::CW1001,
                                    format!("Expected family reference inside `{ident}(...)`"),
                                ));
                            };
                            if !matches!(self.next(), Some(Token::RParen)) {
                                return Err(Diagnostic::error(
                                    DiagnosticCode::CW1001,
                                    format!("Expected `)` after `{ident}({arg}`"),
                                ));
                            }
                            let claim_ref = ClaimRef::parse(&arg)?;
                            match claim_ref {
                                ClaimRef::Family(fref) => Ok(Expr::Reduction { op, target: fref }),
                                ClaimRef::Scalar(_) => Err(Diagnostic::error(
                                    DiagnosticCode::CW2004,
                                    format!(
                                        "Reduction `{ident}(...)` requires a family reference with a selector, got `{arg}`"
                                    ),
                                )),
                            }
                        }
                        "prefix_sum" => {
                            let Some(Token::IdentOrRef(arg)) = self.next() else {
                                return Err(Diagnostic::error(
                                    DiagnosticCode::CW1001,
                                    "Expected metric name inside `prefix_sum(...)`",
                                ));
                            };
                            if !matches!(self.next(), Some(Token::RParen)) {
                                return Err(Diagnostic::error(
                                    DiagnosticCode::CW1001,
                                    format!("Expected `)` after `prefix_sum({arg}`"),
                                ));
                            }
                            if let Some(rest) = arg.strip_prefix('@') {
                                if let Some((ep, fld)) = rest.split_once('.') {
                                    return Ok(Expr::PrefixSum {
                                        epoch: Some(ep.to_string()),
                                        field: fld.to_string(),
                                    });
                                }
                            }
                            Ok(Expr::PrefixSum {
                                epoch: None,
                                field: arg,
                            })
                        }
                        "round" => {
                            let inner = self.parse_or()?;
                            if !matches!(self.next(), Some(Token::Comma)) {
                                return Err(Diagnostic::error(
                                    DiagnosticCode::CW1001,
                                    "Expected `,` in `round(expr, decimals)`",
                                ));
                            }
                            let Some(Token::Number(dec)) = self.next() else {
                                return Err(Diagnostic::error(
                                    DiagnosticCode::CW1001,
                                    "Expected integer decimals in `round(expr, decimals)`",
                                ));
                            };
                            if !matches!(self.next(), Some(Token::RParen)) {
                                return Err(Diagnostic::error(
                                    DiagnosticCode::CW1001,
                                    "Expected `)` after `round(expr, decimals`",
                                ));
                            }
                            Ok(Expr::Round {
                                expr: Box::new(inner),
                                decimals: dec as usize,
                            })
                        }
                        other => Err(Diagnostic::error(
                            DiagnosticCode::CW1001,
                            format!("Unknown function `{other}(...)`"),
                        )),
                    }
                } else if let Some(rest) = ident.strip_prefix('@') {
                    if let Some((ep, fld)) = rest.split_once('.') {
                        Ok(Expr::EpochField {
                            epoch: ep.to_string(),
                            field: fld.to_string(),
                        })
                    } else {
                        Err(Diagnostic::error(
                            DiagnosticCode::CW1001,
                            format!("Invalid `@epoch.field` reference `{ident}`"),
                        ))
                    }
                } else {
                    let claim_ref = ClaimRef::parse(&ident)?;
                    Ok(Expr::Ref(claim_ref))
                }
            }
            other => Err(Diagnostic::error(
                DiagnosticCode::CW1001,
                format!("Unexpected token `{other:?}` in expression"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_expressions() {
        assert!(parse_expr("cycles / prove_wall").is_ok());
        assert!(parse_expr("guest_bench@initial[workload, test].prove_wall / prove_wall").is_ok());
        assert!(parse_expr("duration / sum(phase_bench[*].duration)").is_ok());
        assert!(
            parse_expr("sum(phase_bench[{register_twist, ram_twist, bytecode_shout}].duration)")
                .is_ok()
        );
        assert!(
            parse_expr("min(soundness_ledger[*].per_round_bits) >= paper.t22.soundness_bits")
                .is_ok()
        );
    }
}
