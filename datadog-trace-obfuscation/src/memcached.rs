// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Obfuscates the memcached command cmd.
pub fn obfuscate_memcached_string(cmd: &str) -> String {
    // All memcached commands end with new lines [1]. In the case of storage
    // commands, key values follow after. Knowing this, all we have to do
    // to obfuscate sensitive information is to remove everything that follows
    // a new line. For non-storage commands, this will have no effect.
    // [1]: https://github.com/memcached/memcached/blob/master/doc/protocol.txt
    let split: Vec<&str> = cmd.splitn(2, "\r\n").collect();
    if let Some(res) = split.first() {
        res.to_string()
    } else {
        cmd.to_string()
    }
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
    )]
    #[test]
    fn test_name() {
        let result = obfuscate_memcached_string(input);
        assert_eq!(result, expected);
    }
}
