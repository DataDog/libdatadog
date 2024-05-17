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
    pub fn new(query: &str) -> RedisTokenizer {
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
            self.skip_whitespace();
            if self.curr_char() != b'\n' {
                break;
            }
            self.state = RedisTokenType::RedisTokenCommand;
            self.offset += 1;
        }
        s
    }

    fn next_cmd(&mut self) -> (usize, usize) {
        self.skip_whitespace();
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
                b'\\' => {
                    if !escape {
                        escape = true;
                        self.offset += 1;
                        continue;
                    }
                }
                b'"' => {
                    if !escape {
                        quote = !quote
                    }
                }
                b'\n' => {
                    if !quote {
                        let span = (start, self.offset);
                        self.offset += 1;
                        self.state = RedisTokenType::RedisTokenCommand;
                        return span;
                    }
                }
                b' ' => {
                    if !quote {
                        return (start, self.offset);
                    }
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
        [
            test_name   [test_redis_tokenizer_1]
            input       [""]
            expected    [[r#"{ token: "", token_type: RedisTokenCommand, done: true }"#]];
        ]
        [
            test_name   [test_redis_tokenizer_2]
            input       ["BAD\"\"INPUT\" \"boo\n  Weird13\\Stuff"]
            expected    [
                [
                    r#"{ token: "BAD\"\"INPUT\"", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"boo\n  Weird13\\Stuff", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
        ]
        [
            test_name   [test_redis_tokenizer_3]
            input       ["CMD"]
            expected    [[r#"{ token: "CMD", token_type: RedisTokenCommand, done: true }"#]];
        ]
        [
            test_name   [test_redis_tokenizer_4]
            input       ["\n  \nCMD\n  \n"]
            expected    [[r#"{ token: "CMD", token_type: RedisTokenCommand, done: true }"#]];
        ]
        [
            test_name   [test_redis_tokenizer_5]
            input       ["  CMD  "]
            expected    [[r#"{ token: "CMD", token_type: RedisTokenCommand, done: true }"#]];
        ]
        [
            test_name   [test_redis_tokenizer_6]
            input       ["CMD1\nCMD2"]
            expected    [
                [
                    r#"{ token: "CMD1", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "CMD2", token_type: RedisTokenCommand, done: true }"#
                ]
            ];
        ]
        [
            test_name   [test_redis_tokenizer_7]
            input       ["  CMD1  \n  CMD2  "]
            expected    [
                [
                    r#"{ token: "CMD1", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "CMD2", token_type: RedisTokenCommand, done: true }"#
                ]
            ];
        ]
        [
            test_name   [test_redis_tokenizer_8]
            input       ["CMD1\nCMD2\nCMD3"]
            expected    [
                [
                    r#"{ token: "CMD1", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "CMD2", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "CMD3", token_type: RedisTokenCommand, done: true }"#
                ]
            ];
        ]
        [
            test_name   [test_redis_tokenizer_9]
            input       ["CMD arg"]
            expected    [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "arg", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
        ]
        [
            test_name   [test_redis_tokenizer_10]
            input       ["  CMD  arg  "]
            expected    [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "arg", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
        ]
        [
            test_name   [test_redis_tokenizer_11]
            input       ["CMD arg1 arg2"]
            expected    [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "arg1", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "arg2", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
        ]
        [
            test_name   [test_redis_tokenizer_12]
            input       [" 	 CMD   arg1 	  arg2 "]
            expected    [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "arg1", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "arg2", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
        ]
        [
            test_name   [test_redis_tokenizer_13]
            input       ["CMD arg1\nCMD2 arg2"]
            expected    [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "arg1", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "CMD2", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "arg2", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
        ]
        [
            test_name   [test_redis_tokenizer_14]
            input       ["CMD arg1 arg2\nCMD2 arg3\nCMD3\nCMD4 arg4 arg5 arg6"]
            expected    [
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
        ]
        [
            test_name   [test_redis_tokenizer_15]
            input       ["CMD arg1   arg2  \n CMD2  arg3 \n CMD3 \n  CMD4 arg4 arg5 arg6\nCMD5 "]
            expected    [
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
        ]
        [
            test_name   [test_redis_tokenizer_16]
            input       [r#"CMD """#]
            expected    [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"\"", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
        ]
        [
            test_name   [test_redis_tokenizer_17]
            input       [r#"CMD "foo bar""#]
            expected    [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"foo bar\"", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
        ]
        [
            test_name   [test_redis_tokenizer_18]
            input       [r#"CMD "foo bar\ " baz"#]
            expected    [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"foo bar\\ \"", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "baz", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
        ]
        [
            test_name   [test_redis_tokenizer_19]
            input       ["CMD \"foo \n bar\" \"\"  baz "]
            expected    [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"foo \n bar\"", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "\"\"", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "baz", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
        ]
        [
            test_name   [test_redis_tokenizer_20]
            input       ["CMD \"foo \\\" bar\" baz"]
            expected    [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"foo \\\" bar\"", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "baz", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
        ]
        [
            test_name   [test_redis_tokenizer_21]
            input       [r#"CMD "foo bar" baz"#]
            expected    [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"foo bar\"", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "baz", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
        ]
        [
            test_name   [test_redis_tokenizer_22]
            input       ["CMD \"foo bar\" baz\nCMD2 \"baz\\\\bar\""]
            expected    [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"foo bar\"", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "baz", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "CMD2", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"baz\\\\bar\"", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
        ]
        [
            test_name   [test_redis_tokenizer_23]
            input       [" CMD  \"foo bar\"  baz \n CMD2  \"baz\\\\bar\"  "]
            expected    [
                [
                    r#"{ token: "CMD", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"foo bar\"", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "baz", token_type: RedisTokenArgument, done: false }"#,
                    r#"{ token: "CMD2", token_type: RedisTokenCommand, done: false }"#,
                    r#"{ token: "\"baz\\\\bar\"", token_type: RedisTokenArgument, done: true }"#
                ]
            ];
        ]
    )]
    #[test]
    fn test_name() {
        let mut tokenizer = RedisTokenizer::new(input);
        for i in 0..expected.len() {
            let res = tokenizer.scan();
            assert_eq!(
                format!("{:?}", res),
                format!("RedisTokenizerScanResult {}", expected[i])
            );
        }
    }
}
