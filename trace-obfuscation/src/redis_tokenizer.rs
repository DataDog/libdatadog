// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

#[derive(Debug)]
enum RedisTokenType {
    RedisTokenCommand,
    RedisTokenArgument,
}

pub struct RedisTokenizer {
    data: Vec<char>,
    cur_char: char,
    offset: Option<usize>,
    done: bool,
    state: RedisTokenType, // specifies the token we are about to parse
}

#[derive(Debug)]
pub struct RedisTokenizerScanResult {
    token: String,
    token_type: RedisTokenType,
    done: bool,
}

impl RedisTokenizer {
    pub fn new(query: &str) -> RedisTokenizer {
        RedisTokenizer {
            data: query.trim().chars().collect(),
            cur_char: ' ',
            offset: None,
            done: false,
            state: RedisTokenType::RedisTokenCommand,
        }
    }

    pub fn scan(&mut self) -> RedisTokenizerScanResult {
        match self.state {
            RedisTokenType::RedisTokenCommand => self.scan_command(),
            RedisTokenType::RedisTokenArgument => self.scan_argument(),
        }
    }

    fn scan_command(&mut self) -> RedisTokenizerScanResult {
        let s = &mut String::new();
        let mut started = false;
        loop {
            self.next();
            if self.done {
                return RedisTokenizerScanResult {
                    token: s.to_string(),
                    token_type: RedisTokenType::RedisTokenCommand,
                    done: self.done,
                };
            }
            match self.cur_char {
                ' ' => {
                    if !started {
                        self.skip_space();
                        continue;
                    }
                    // done scanning command
                    self.state = RedisTokenType::RedisTokenArgument;
                    self.skip_space();
                    return RedisTokenizerScanResult {
                        token: s.to_string(),
                        token_type: RedisTokenType::RedisTokenCommand,
                        done: self.done,
                    };
                }
                '\n' => {
                    return RedisTokenizerScanResult {
                        token: s.to_string(),
                        token_type: RedisTokenType::RedisTokenCommand,
                        done: self.done,
                    };
                }
                _ => {
                    s.push(self.cur_char);
                }
            }
            started = true
        }
    }

    fn skip_space(&mut self) {
        while matches!(self.cur_char, ' ' | '\t') || self.cur_char == '\r' && !self.done {
            self.next()
        }
        if self.cur_char == '\n' {
            // next token is a command
            self.state = RedisTokenType::RedisTokenCommand
        } else {
            // don't steal the first non-space character
            self.unread()
        }
    }

    fn scan_argument(&mut self) -> RedisTokenizerScanResult {
        let s = &mut String::new();
        let mut quoted = false; // in quoted string
        let mut escape = false; // in escape sequence
        loop {
            self.next();
            if self.done {
                return RedisTokenizerScanResult {
                    token: s.to_string(),
                    token_type: RedisTokenType::RedisTokenArgument,
                    done: self.done,
                };
            }
            match self.cur_char {
                '\\' => {
                    s.push('\\');
                    if !escape {
                        escape = true;
                        continue;
                    }
                }
                '\n' => {
                    if !quoted {
                        self.state = RedisTokenType::RedisTokenCommand;
                        return RedisTokenizerScanResult {
                            token: s.to_string(),
                            token_type: RedisTokenType::RedisTokenArgument,
                            done: self.done,
                        };
                    }
                    s.push('\n')
                }
                '"' => {
                    s.push('"');
                    if !escape {
                        // this quote wasn't escaped, toggle quoted mode
                        quoted = !quoted
                    }
                }
                ' ' => {
                    if !quoted {
                        self.skip_space();
                        return RedisTokenizerScanResult {
                            token: s.to_string(),
                            token_type: RedisTokenType::RedisTokenArgument,
                            done: self.done,
                        };
                    }
                    s.push(' ');
                }
                _ => {
                    s.push(self.cur_char);
                }
            }
            escape = false
        }
    }

    fn next(&mut self) {
        if let Some(offset) = self.offset {
            self.offset = Some(offset + 1);
        } else {
            self.offset = Some(0);
        }
        let offset = self.offset.unwrap();
        if offset < self.data.len() {
            self.cur_char = self.data[offset];
            return;
        }
        self.done = true;
    }

    fn unread(&mut self) {
        if let Some(offset) = self.offset {
            if offset < 1 {
                return;
            }
            self.offset = Some(offset - 1);
            self.cur_char = self.data[offset - 1]
        }
    }
}

#[cfg(test)]
mod tests {
    use duplicate::duplicate_item;

    use super::{RedisTokenizer, RedisTokenizerScanResult};

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
