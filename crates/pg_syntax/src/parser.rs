use pg_lexer::TokenType;
use pg_lexer::{SyntaxKind, Token, WHITESPACE_TOKENS};
use std::ops::Range;

pub enum ParserEvent<'a> {
    Token(&'a Token),
    StartNode(pg_query_ext::NodeEnum),
    FinishNode,
}

pub trait EventSink {
    fn push(&mut self, event: ParserEvent);
}

/// Main parser that exposes the `cstree` api, and collects errors and statements
pub struct Parser<'p> {
    event_sink: Option<&'p mut dyn EventSink>,
    /// The tokens to parse
    pub tokens: Vec<Token>,
    /// The current position in the token stream
    pub pos: usize,
    /// index from which whitespace tokens are buffered
    pub whitespace_token_buffer: Option<usize>,
    /// index from which tokens are buffered
    token_buffer: Option<usize>,

    pub depth: usize,

    eof_token: Token,
}

#[allow(dead_code)]
impl<'p> Parser<'p> {
    pub fn new(tokens: Vec<Token>, event_sink: Option<&'p mut dyn EventSink>) -> Self {
        Self {
            event_sink,
            eof_token: Token::eof(usize::from(tokens.last().unwrap().span.end())),
            tokens,
            pos: 0,
            whitespace_token_buffer: None,
            token_buffer: None,
            depth: 0,
        }
    }

    pub fn token_range(&self) -> Range<usize> {
        0..self.tokens.len()
    }

    /// start a new node of `SyntaxKind`
    pub fn start_node(&mut self, kind: pg_query_ext::NodeEnum) {
        self.flush_token_buffer();
        if let Some(ref mut event_sink) = self.event_sink {
            (*event_sink).push(ParserEvent::StartNode(kind));
        }
        self.depth += 1;
    }
    /// finish current node
    pub fn finish_node(&mut self) {
        if let Some(ref mut event_sink) = self.event_sink {
            (*event_sink).push(ParserEvent::FinishNode);
        }
        self.depth -= 1;
    }

    /// Opens a buffer for tokens. While the buffer is active, tokens are not applied to the tree.
    pub fn open_buffer(&mut self) {
        self.token_buffer = Some(self.pos);
    }

    /// Closes the current token buffer, resets the position to the start of the buffer and returns the range of buffered tokens.
    pub fn close_buffer(&mut self) -> Range<usize> {
        let token_buffer = self.token_buffer.unwrap();
        let token_range = token_buffer..self.whitespace_token_buffer.unwrap_or(self.pos);
        self.token_buffer = None;
        self.pos = token_buffer;
        token_range
    }

    /// applies token and advances
    pub fn advance(&mut self) {
        assert!(!self.eof());
        if self.nth(0, false).kind == SyntaxKind::Whitespace {
            if self.whitespace_token_buffer.is_none() {
                self.whitespace_token_buffer = Some(self.pos);
            }
        } else {
            self.flush_token_buffer();
            if self.token_buffer.is_none() {
                let token = self.tokens.get(self.pos).unwrap();
                if let Some(ref mut event_sink) = self.event_sink {
                    (*event_sink).push(ParserEvent::Token(token));
                }
            }
        }
        self.pos += 1;
    }

    /// flush token buffer and applies all tokens
    pub fn flush_token_buffer(&mut self) {
        if self.whitespace_token_buffer.is_none() {
            return;
        }
        while self.whitespace_token_buffer.unwrap() < self.pos {
            let token = self
                .tokens
                .get(self.whitespace_token_buffer.unwrap())
                .unwrap();
            if self.token_buffer.is_none() {
                if let Some(ref mut event_sink) = self.event_sink {
                    (*event_sink).push(ParserEvent::Token(token));
                }
            }
            self.whitespace_token_buffer = Some(self.whitespace_token_buffer.unwrap() + 1);
        }
        self.whitespace_token_buffer = None;
    }

    pub fn eat(&mut self, kind: SyntaxKind) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    pub fn at_whitespace(&self) -> bool {
        self.nth(0, false).kind == SyntaxKind::Whitespace
    }

    pub fn eat_whitespace(&mut self) {
        while self.nth(0, false).token_type == TokenType::Whitespace {
            self.advance();
        }
    }

    pub fn eof(&self) -> bool {
        self.pos == self.tokens.len()
    }

    /// lookahead method.
    ///
    /// if `ignore_whitespace` is true, it will skip all whitespace tokens
    pub fn nth(&self, lookahead: usize, ignore_whitespace: bool) -> &Token {
        if ignore_whitespace {
            let mut idx = 0;
            let mut non_whitespace_token_ctr = 0;
            loop {
                match self.tokens.get(self.pos + idx) {
                    Some(token) => {
                        if !WHITESPACE_TOKENS.contains(&token.kind) {
                            if non_whitespace_token_ctr == lookahead {
                                return token;
                            }
                            non_whitespace_token_ctr += 1;
                        }
                        idx += 1;
                    }
                    None => {
                        return &self.eof_token;
                    }
                }
            }
        } else {
            match self.tokens.get(self.pos + lookahead) {
                Some(token) => token,
                None => &self.eof_token,
            }
        }
    }

    /// checks if the current token is any of `kinds`
    pub fn at_any(&self, kinds: &[SyntaxKind]) -> bool {
        kinds.iter().any(|&it| self.at(it))
    }

    /// checks if the current token is of `kind`
    pub fn at(&self, kind: SyntaxKind) -> bool {
        self.nth(0, false).kind == kind
    }

    /// like at, but for multiple consecutive tokens
    pub fn at_all(&self, kinds: &[SyntaxKind]) -> bool {
        kinds
            .iter()
            .enumerate()
            .all(|(idx, &it)| self.nth(idx, false).kind == it)
    }

    /// like at_any, but for multiple consecutive tokens
    pub fn at_any_all(&self, kinds: &Vec<&[SyntaxKind]>) -> bool {
        kinds.iter().any(|&it| self.at_all(it))
    }
}
