// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[derive(Debug, Clone, Copy)]
pub enum RedisTokenType {
    RedisTokenCommand,
    RedisTokenArgument,
}

pub struct RedisTokenizer<'a> {
    data: &'a str,
    offset: usize,
    state: RedisTokenType, // specifies the token we are about to parse
}

#[derive(Debug)]
pub struct RedisTokenizerScanResult<'a> {
    pub token: &'a str,
    pub token_type: RedisTokenType,
    pub done: bool,
}

impl<'a> RedisTokenizer<'a> {
    #[must_use]
    pub fn new(query: &str) -> RedisTokenizer<'_> {
        let mut s = RedisTokenizer {
            data: query,
            offset: 0,
            state: RedisTokenType::RedisTokenCommand,
        };
        s.skip_empty_lines();
        s
    }

    pub fn scan(&mut self) -> RedisTokenizerScanResult<'a> {
        let token_type = self.state;
        let current = self.next_token();
        RedisTokenizerScanResult {
            token: &self.data[current.0..current.1],
            token_type,
            done: self.curr_char() == 0,
        }
    }

    pub fn next_token(&mut self) -> (usize, usize) {
        let s = match self.state {
            RedisTokenType::RedisTokenCommand => self.next_cmd(),
            RedisTokenType::RedisTokenArgument => self.next_arg(),
        };
        loop {
            // Only skip spaces between commands (not tabs - Go only skips spaces)
            while self.curr_char() == b' ' {
                self.offset += 1;
            }
            if self.curr_char() != b'\n' {
                break;
            }
            self.state = RedisTokenType::RedisTokenCommand;
            self.offset += 1;
        }
        s
    }

    fn next_cmd(&mut self) -> (usize, usize) {
        // Go's scanCommand only skips ASCII spaces before the command (not tabs).
        // Tabs are included in the command token (default case in Go's switch).
        while self.curr_char() == b' ' {
            self.offset += 1;
        }
        let start = self.offset;
        loop {
            match self.curr_char() {
                0 => break,
                b'\n' => {
                    let span = (start, self.offset);
                    self.offset += 1;
                    return span;
                }
                b' ' => {
                    self.state = RedisTokenType::RedisTokenArgument;
                    break;
                }
                _ => self.offset += 1,
            }
        }
        (start, self.offset)
    }

    fn next_arg(&mut self) -> (usize, usize) {
        self.skip_whitespace();
        let start = self.offset;
        let mut quote = false;
        let mut escape = false;
        loop {
            match self.curr_char() {
                0 => break,
                b'\\' if !escape => {
                    escape = true;
                    self.offset += 1;
                    continue;
                }
                b'"' if !escape => quote = !quote,
                b'\n' if !quote => {
                    let span = (start, self.offset);
                    self.offset += 1;
                    self.state = RedisTokenType::RedisTokenCommand;
                    return span;
                }
                b' ' if !quote => {
                    return (start, self.offset);
                }
                _ => {}
            }
            escape = false;
            self.offset += 1;
        }
        (start, self.offset)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.curr_char(), b' ' | b'\t' | b'\r') {
            self.offset += 1;
        }
    }

    fn skip_empty_lines(&mut self) {
        while matches!(self.curr_char(), b' ' | b'\t' | b'\r' | b'\n') {
            self.offset += 1;
        }
    }

    fn curr_char(&self) -> u8 {
        match self.data.as_bytes().get(self.offset) {
            Some(&c) => c,
            None => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use duplicate::duplicate_item;

    use super::RedisTokenizer;

    #[duplicate_item(
    test_name input expected;
    [test_redis_tokenizer_1] [""] [[r#"{ token: "", token_type: RedisTokenCommand, done: true }"#]];
    [test_redis_tokenizer_2] ["BAD\"\"INPUT\" \"boo\n  Weird13\\Stuff"] [
                [
                    r#"{ token: "BAD\"\"INPUT\"", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"boo\n  Weird13\\Stuff", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
    [test_redis_tokenizer_3] ["CMD"] [[r#"{ token: "CMD", token_type: RedisTokenCommand, done: true }"#]];
    [test_redis_tokenizer_4] ["\n  \nCMD\n  \n"] [[r#"{ token: "CMD", token_type: RedisTokenCommand, done: true }"#]];
    [test_redis_tokenizer_5] ["  CMD  "] [[r#"{ token: "CMD", token_type: RedisTokenCommand, done: true }"#]];
    [test_redis_tokenizer_6] ["CMD1\nCMD2"] [
                [
                    r#"{ token: "CMD1", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "CMD2", token_type: RedisTokenCommand, done: true }"#
                ]
            ];
    [test_redis_tokenizer_7] ["  CMD1  \n  CMD2  "] [
                [
                    r#"{ token: "CMD1", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "CMD2", token_type: RedisTokenCommand, done: true }"#
                ]
            ];
    [test_redis_tokenizer_8] ["CMD1\nCMD2\nCMD3"] [
                [
                    r#"{ token: "CMD1", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "CMD2", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "CMD3", token_type: RedisTokenCommand, done: true }"#
                ]
            ];
    [test_redis_tokenizer_9] ["CMD arg"] [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "arg", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
    [test_redis_tokenizer_10] ["  CMD  arg  "] [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "arg", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
    [test_redis_tokenizer_11] ["CMD arg1 arg2"] [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "arg1", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "arg2", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
    [test_redis_tokenizer_12] [" 	 CMD   arg1 	  arg2 "] [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "arg1", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "arg2", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
    [test_redis_tokenizer_13] ["CMD arg1\nCMD2 arg2"] [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "arg1", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "CMD2", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "arg2", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
    [test_redis_tokenizer_14] ["CMD arg1 arg2\nCMD2 arg3\nCMD3\nCMD4 arg4 arg5 arg6"] [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "arg1", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "arg2", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "CMD2", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "arg3", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "CMD3", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "CMD4", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "arg4", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "arg5", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "arg6", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
    [test_redis_tokenizer_15] ["CMD arg1   arg2  \n CMD2  arg3 \n CMD3 \n  CMD4 arg4 arg5 arg6\nCMD5 "] [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "arg1", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "arg2", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "CMD2", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "arg3", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "CMD3", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "CMD4", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "arg4", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "arg5", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "arg6", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "CMD5", token_type: RedisTokenCommand, done: true }"#,
                ]
            ];
    [test_redis_tokenizer_16] [r#"CMD """#] [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"\"", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
    [test_redis_tokenizer_17] [r#"CMD "foo bar""#] [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"foo bar\"", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
    [test_redis_tokenizer_18] [r#"CMD "foo bar\ " baz"#] [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"foo bar\\ \"", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "baz", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
    [test_redis_tokenizer_19] ["CMD \"foo \n bar\" \"\"  baz "] [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"foo \n bar\"", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "\"\"", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "baz", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
    [test_redis_tokenizer_20] ["CMD \"foo \\\" bar\" baz"] [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"foo \\\" bar\"", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "baz", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
    [test_redis_tokenizer_21] [r#"CMD "foo bar" baz"#] [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"foo bar\"", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "baz", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
    [test_redis_tokenizer_22] ["CMD \"foo bar\" baz\nCMD2 \"baz\\\\bar\""] [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"foo bar\"", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "baz", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "CMD2", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"baz\\\\bar\"", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
    [test_redis_tokenizer_23] [" CMD  \"foo bar\"  baz \n CMD2  \"baz\\\\bar\"  "] [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"foo bar\"", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "baz", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "CMD2", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"baz\\\\bar\"", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
    )]
    #[test]
    fn test_name() {
        let mut tokenizer = RedisTokenizer::new(input);
        for i in 0..expected.len() {
            let res = tokenizer.scan();
            assert_eq!(
                format!("{res:?}"),
                format!("RedisTokenizerScanResult {}", expected[i])
            );
        }
    }
}
