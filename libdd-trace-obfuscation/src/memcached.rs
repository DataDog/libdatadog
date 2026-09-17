// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Obfuscates a memcached command, returning `None` when nothing needs to change.
///
/// With `keep_command` false the command is wiped to `""`, or `None` if already empty. With it
/// true, obfuscation only strips everything after the first `\r\n` and trims whitespace, so it
/// returns `None` when there is no `\r\n` and no surrounding whitespace.
#[must_use]
pub fn obfuscate_memcached(cmd: &str, keep_command: bool) -> Option<String> {
    if !keep_command {
        return if cmd.is_empty() {
            None
        } else {
            Some(String::new())
        };
    }
    let needs_trim = cmd.starts_with(char::is_whitespace) || cmd.ends_with(char::is_whitespace);
    if !cmd.contains("\r\n") && !needs_trim {
        return None;
    }
    Some(obfuscate_memcached_string(cmd))
}

/// Obfuscates the memcached command cmd.
#[must_use]
pub fn obfuscate_memcached_string(cmd: &str) -> String {
    // All memcached commands end with new lines [1]. In the case of storage
    // commands, key values follow after. Knowing this, all we have to do
    // to obfuscate sensitive information is to remove everything that follows
    // a new line. For non-storage commands, this will have no effect.
    // [1]: https://github.com/memcached/memcached/blob/master/doc/protocol.txt
    let (cmd, _rest) = cmd.split_once("\r\n").unwrap_or((cmd, ""));
    cmd.trim().to_string()
}

#[cfg(test)]
mod tests {
    use duplicate::duplicate_item;

    use super::obfuscate_memcached_string;

    #[duplicate_item(
        test_name                       input                                       expected;
        [test_obfuscate_memcached_1]    ["set mykey 0 60 5\r\nvalue"]               ["set mykey 0 60 5"];
        [test_obfuscate_memcached_2]    ["get mykey"]                               ["get mykey"];
        [test_obfuscate_memcached_3]    ["add newkey 0 60 5\r\nvalue"]              ["add newkey 0 60 5"];
        [test_obfuscate_memcached_4]    ["add newkey 0 60 5\r\nvalue\r\nvalue1"]    ["add newkey 0 60 5"];
        [test_obfuscate_memcached_5]    ["decr mykey 5"]                            ["decr mykey 5"];
        [fuzzing_2126976840]            ["\t"]                                      [""];
    )]
    #[test]
    fn test_name() {
        let result = obfuscate_memcached_string(input);
        assert_eq!(result, expected);
    }
}
