// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::redis_tokenizer::{RedisTokenType, RedisTokenizer};

const REDIS_TRUNCATION_MARK: &str = "...";
const MAX_REDIS_NB_COMMANDS: usize = 3;

/// Uppercase a single char to match Go's unicode.ToUpper (Unicode 15.0).
/// Rust uses Unicode 16.0 which added case pairs for U+16E80–U+16EFF (Bamum Supplement)
/// that Go 1.25 doesn't know about — keep those chars unchanged to match Go.
fn go_toupper(c: char) -> char {
    if (0x16E80..=0x16EFF).contains(&(c as u32)) {
        return c;
    }
    let mut upper = c.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(u), None) => u,
        _ => c,
    }
}

/// Quantizes a Redis query, returning `None` for empty input.
///
/// The dispatch gate (span type and tag presence) is the real precheck. The tokenizer rewrites
/// any non-empty command, so only empty input is skipped.
#[must_use]
pub fn quantize_redis(query: &str) -> Option<String> {
    if query.is_empty() {
        return None;
    }
    Some(quantize_redis_string(query))
}

/// Obfuscates a Redis command, returning `None` for empty input.
#[must_use]
pub fn obfuscate_redis(cmd: &str) -> Option<String> {
    if cmd.is_empty() {
        return None;
    }
    Some(obfuscate_redis_string(cmd))
}

/// Removes all Redis arguments, returning `None` for empty input.
#[must_use]
pub fn obfuscate_redis_remove_all_args(redis_cmd: &str) -> Option<String> {
    if redis_cmd.is_empty() {
        return None;
    }
    Some(remove_all_redis_args(redis_cmd))
}

/// Returns a quantized version of a Redis query, keeping only up to 3 command names.
pub fn quantize_redis_string(query: &str) -> String {
    let mut commands: Vec<String> = Vec::with_capacity(MAX_REDIS_NB_COMMANDS);
    let mut truncated = false;

    // Split on '\n' only (like Go's strings.IndexByte), preserving '\r' in line content
    for raw_line in query.split('\n') {
        if commands.len() >= MAX_REDIS_NB_COMMANDS {
            break;
        }

        // Go's QuantizeRedisString trims only ASCII spaces (strings.Trim(rawLine, " ")),
        // not all whitespace. Use trim_matches(' ') to match that behavior.
        let line = raw_line.trim_matches(' ');
        if line.is_empty() {
            continue;
        }

        // Go splits on spaces only (strings.SplitN(line, " ", 3)), not all whitespace.
        // Use split(' ').filter to match that behavior and preserve tab tokens.
        let mut tokens = line.split(' ').filter(|s| !s.is_empty());
        let Some(first) = tokens.next() else { continue };

        if first.ends_with(REDIS_TRUNCATION_MARK) {
            truncated = true;
            continue;
        }

        let cmd: String = first.chars().map(go_toupper).collect();
        let command = match cmd.as_bytes() {
            b"CLIENT" | b"CLUSTER" | b"COMMAND" | b"CONFIG" | b"DEBUG" | b"SCRIPT" => {
                match tokens.next() {
                    Some(sub) if sub.ends_with(REDIS_TRUNCATION_MARK) => {
                        truncated = true;
                        continue;
                    }
                    Some(sub) => {
                        format!("{cmd} {}", sub.chars().map(go_toupper).collect::<String>())
                    }
                    None => cmd,
                }
            }
            _ => cmd,
        };

        commands.push(command);
        truncated = false;
    }

    let mut result = commands.join(" ");
    if commands.len() == MAX_REDIS_NB_COMMANDS || truncated {
        if !result.is_empty() {
            result.push(' ');
        }
        result.push_str("...");
    }
    result
}

#[must_use]
pub fn obfuscate_redis_string(cmd: &str) -> String {
    // Go's newRedisTokenizer calls bytes.TrimSpace before tokenizing
    let cmd = cmd.trim();
    let mut tokenizer = RedisTokenizer::new(cmd);
    let mut s = String::new();
    let mut cmd: Option<&str> = None;
    let mut args: Vec<&str> = Vec::new();

    loop {
        let res = tokenizer.scan();
        match res.token_type {
            RedisTokenType::RedisTokenCommand => {
                if let Some(cmd) = cmd {
                    args = obfuscate_redis_cmd(&mut s, cmd, args);
                    s.push('\n');
                }
                cmd = Some(res.token);
                args.clear();
            }
            RedisTokenType::RedisTokenArgument => args.push(res.token),
        }
        if res.done {
            obfuscate_redis_cmd(&mut s, cmd.unwrap_or_default(), args);
            break;
        }
    }
    s
}

fn obfuscate_redis_cmd<'a>(str: &mut String, cmd: &'a str, mut args: Vec<&'a str>) -> Vec<&'a str> {
    str.push_str(cmd);
    if args.is_empty() {
        return args;
    }
    str.push(' ');
    let mut uppercase_cmd = [0; 32]; // no redis cmd is longer than 32 chars
    let uppercase_cmd = ascii_uppercase(cmd, &mut uppercase_cmd).unwrap_or(&[]);
    match uppercase_cmd {
        b"AUTH" | b"MIGRATE" | b"HELLO"
            // Obfuscate everything after command:
            // • AUTH password
            // • MIGRATE host port key|"" destination-db timeout [COPY] [REPLACE] [AUTH password]
            //   [AUTH2 username password] [KEYS key [key ...]]
            // • HELLO [protover [AUTH username password] [SETNAME clientname]]
            if !args.is_empty() => {
                args.clear();
                args.push("?");
            }
        b"ACL" | b"GEOHASH" | b"GEOPOS" | b"GEODIST" | b"LPUSH" | b"RPUSH" | b"SREM" | b"ZREM"
        | b"SADD"
            // Obfuscate all arguments after the first token:
            // • ACL SETUSER username on >password ~keys &channels +commands
            // • ACL GETUSER username
            // • ACL DELUSER username [username ...]
            // • ACL LIST
            // • ACL WHOAMI
            // • GEOHASH key member [member ...]
            // • GEOPOS key member [member ...]
            // • GEODIST key member1 member2 [unit]
            // • LPUSH key value [value ...]
            // • RPUSH key value [value ...]
            // • SREM key member [member ...]
            // • ZREM key member [member ...]
            // • SADD key member [member ...]
            if args.len() > 1 => {
                args[1] = "?";
                args.drain(2..);
            }
        b"APPEND" | b"GETSET" | b"LPUSHX" | b"GEORADIUSBYMEMBER" | b"RPUSHX" | b"SET"
        | b"SETNX" | b"SISMEMBER" | b"ZRANK" | b"ZREVRANK" | b"ZSCORE" => {
            // Obfuscate 2nd argument:
            // • APPEND key value
            // • GETSET key value
            // • LPUSHX key value
            // • GEORADIUSBYMEMBER key member radius m|km|ft|mi [WITHCOORD] [WITHDIST] [WITHHASH]
            // [COUNT count] [ASC|DESC] [STORE key] [STOREDIST key]
            // • RPUSHX key value
            // • SET key value [expiration EX seconds|PX milliseconds] [NX|XX]
            // • SETNX key value
            // • SISMEMBER key member
            // • ZRANK key member
            // • ZREVRANK key member
            // • ZSCORE key member
            args = obfuscate_redis_args_n(args, 1);
        }
        b"HSETNX" | b"LREM" | b"LSET" | b"SETBIT" | b"SETEX" | b"PSETEX" | b"SETRANGE"
        | b"ZINCRBY" | b"SMOVE" | b"RESTORE" => {
            // Obfuscate 3rd argument:
            // • HSET key field value
            // • HSETNX key field value
            // • LREM key count value
            // • LSET key index value
            // • SETBIT key offset value
            // • SETEX key seconds value
            // • PSETEX key milliseconds value
            // • SETRANGE key offset value
            // • ZINCRBY key increment member
            // • SMOVE source destination member
            // • RESTORE key ttl serialized-value [REPLACE]
            args = obfuscate_redis_args_n(args, 2);
        }
        b"LINSERT" => {
            // Obfuscate 4th argument:
            // • LINSERT key BEFORE|AFTER pivot value
            args = obfuscate_redis_args_n(args, 3);
        }
        b"GEOADD" => {
            // Obfuscating every 3rd argument starting from first
            // • GEOADD key longitude latitude member [longitude latitude member ...]
            args = obfuscate_redis_args_step(args, 1, 3);
        }
        b"HMSET" | b"HSET" => {
            // Every 2nd argument starting from first.
            // • HMSET key field value [field value ...]
            args = obfuscate_redis_args_step(args, 1, 2);
        }
        b"MSET" | b"MSETNX" => {
            // Every 2nd argument starting from command.
            // • MSET key value [key value ...]
            // • MSETNX key value [key value ...]
            args = obfuscate_redis_args_step(args, 0, 2);
        }
        b"CONFIG" => {
            // Obfuscate 2nd argument to SET sub-command.
            // • CONFIG SET parameter value
            let mut uppercase_arg = [0; 8];
            let uppercase_arg = ascii_uppercase(args[0], &mut uppercase_arg).unwrap_or(b"");
            if uppercase_arg == b"SET" {
                args = obfuscate_redis_args_n(args, 2);
            }
        }
        b"BITFIELD" => {
            // Obfuscate 3rd argument to SET sub-command:
            // • BITFIELD key [GET type offset] [SET type offset value] [INCRBY type offset
            // increment] [OVERFLOW WRAP|SAT|FAIL]
            let mut n = 0;
            for (i, arg) in args.iter_mut().enumerate() {
                let mut uppercase_arg = [0; 8];
                let uppercase_arg = ascii_uppercase(arg, &mut uppercase_arg).unwrap_or(b"");
                if uppercase_arg == b"SET" {
                    n = i;
                }
                if n > 0 && i - n == 3 {
                    *arg = "?";
                    break;
                }
            }
        }
        b"ZADD" => {
            for i in 0..args.len() {
                if i == 0 {
                    continue; // key
                }
                let mut uppercase_arg = [0; 8];
                let uppercase_arg = ascii_uppercase(args[i], &mut uppercase_arg).unwrap_or(b"");
                match uppercase_arg {
                    b"NX" | b"XX" | b"CH" | b"INCR" => {}
                    _ => {
                        args = obfuscate_redis_args_step(args, i, 2);
                        break;
                    }
                }
            }
        }
        _ => {}
    }
    str.push_str(&args.join(" "));
    args
}

fn obfuscate_redis_args_n(mut args: Vec<&str>, n: usize) -> Vec<&str> {
    if args.len() > n {
        args[n] = "?";
    }
    args
}

fn obfuscate_redis_args_step(mut args: Vec<&str>, start: usize, step: usize) -> Vec<&str> {
    if start + step > args.len() {
        return args;
    }
    for i in ((start + step - 1)..args.len()).step_by(step) {
        args[i] = "?";
    }
    args
}

#[must_use]
pub fn remove_all_redis_args(redis_cmd: &str) -> String {
    let mut redis_cmd_iter = redis_cmd.split_whitespace().peekable();
    let mut obfuscated_cmd = String::new();

    // If the redis command is empty, return immediately. Otherwise, store the command token.
    let Some(cmd) = redis_cmd_iter.next() else {
        return obfuscated_cmd;
    };
    obfuscated_cmd.push_str(cmd);

    // If there are no tokens left in the iterator, return the obfuscated result with just the
    // command.
    if redis_cmd_iter.peek().is_none() {
        return obfuscated_cmd;
    }

    obfuscated_cmd.push(' ');

    let mut uppercase_cmd = [0; 32];
    let uppercase_cmd = ascii_uppercase(cmd, &mut uppercase_cmd).unwrap_or(&[]);
    match uppercase_cmd {
        b"BITFIELD" => {
            obfuscated_cmd.push('?');
            for a in redis_cmd_iter {
                let mut uppercase_arg = [0; 8];
                let uppercase_arg = ascii_uppercase(a, &mut uppercase_arg).unwrap_or(b"");
                if uppercase_arg == b"SET" || uppercase_arg == b"GET" || uppercase_arg == b"INCRBY"
                {
                    obfuscated_cmd.push_str(format!(" {a} ?").as_str());
                }
            }
        }
        b"CONFIG" => {
            let a = redis_cmd_iter.next().unwrap_or_default();
            let mut uppercase_arg = [0; 16];
            let uppercase_arg = ascii_uppercase(a, &mut uppercase_arg).unwrap_or(b"");
            if uppercase_arg == b"GET"
                || uppercase_arg == b"SET"
                || uppercase_arg == b"RESETSTAT"
                || uppercase_arg == b"REWRITE"
            {
                obfuscated_cmd.push_str(format!("{a} ?").as_str());
            } else {
                obfuscated_cmd.push('?');
            }
        }
        _ => {
            obfuscated_cmd.push('?');
        }
    }

    obfuscated_cmd
}

fn ascii_uppercase<'a>(s: &str, dest: &'a mut [u8]) -> Option<&'a [u8]> {
    if s.len() > dest.len() {
        return None;
    }
    for (i, c) in s.as_bytes().iter().enumerate() {
        if c.is_ascii() {
            dest[i] = c.to_ascii_uppercase();
        }
    }
    Some(&dest[0..s.len()])
}

#[cfg(test)]
mod tests {
    use duplicate::duplicate_item;

    use super::{obfuscate_redis_string, quantize_redis_string, remove_all_redis_args};

    #[duplicate_item(
    test_name input expected;
    [test_quantize_redis_string_client] ["CLIENT"] ["CLIENT"];
    [test_quantize_redis_string_client_list] ["CLIENT LIST"] ["CLIENT LIST"];
    [test_quantize_redis_string_client_truncated] ["CLIENT ..."] ["..."];
    [test_quantize_redis_string_get_lowercase] ["get my_key"] ["GET"];
    [test_quantize_redis_string_set] ["SET le_key le_value"] ["SET"];
    [test_quantize_redis_string_set_with_newlines] ["\n\n  \nSET foo bar  \n  \n\n  "] ["SET"];
    [test_quantize_redis_string_config_set] ["CONFIG SET parameter value"] ["CONFIG SET"];
    [test_quantize_redis_string_two_cmds] ["SET toto tata \n \n  EXPIRE toto 15  "] ["SET EXPIRE"];
    [test_quantize_redis_string_mset] ["MSET toto tata toto tata toto tata \n "] ["MSET"];
    [test_quantize_redis_string_max_cmds] ["MULTI\nSET k1 v1\nSET k2 v2\nSET k3 v3\nSET k4 v4\nDEL to_del\nEXEC"] ["MULTI SET SET ..."];
    [test_quantize_redis_string_truncation_first] ["GET..."] ["..."];
    [test_quantize_redis_string_truncation_arg] ["GET k..."] ["GET"];
    [test_quantize_redis_string_truncation_third] ["GET k1\nGET k2\nG..."] ["GET GET ..."];
    [test_quantize_redis_string_truncation_after_max] ["GET k1\nGET k2\nDEL k3\nGET k..."] ["GET GET DEL ..."];
    [test_quantize_redis_string_truncation_hdel] ["GET k1\nGET k2\nHDEL k3 a\nG..."] ["GET GET HDEL ..."];
    [test_quantize_redis_string_truncation_mid] ["GET k...\nDEL k2\nMS..."] ["GET DEL ..."];
    [test_quantize_redis_string_truncation_early] ["GET k...\nDE...\nMS..."] ["GET ..."];
    [test_quantize_redis_string_truncation_then_cmd] ["GET k1\nDE...\nGET k2"] ["GET GET"];
    [test_quantize_redis_string_truncation_complex] ["GET k1\nDE...\nGET k2\nHDEL k3 a\nGET k4\nDEL k5"] ["GET GET HDEL ..."];
    [test_quantize_redis_string_unknown] ["UNKNOWN 123"] ["UNKNOWN"];
    [fuzzing_3286489773] ["ꭺ"] ["Ꭺ"];
    [fuzzing_2812552373] ["\t"] ["\t"];
    [fuzzing_crlf] ["\r\n"] ["\r"];
    [fuzzing_box_char] ["𖺻"] ["𖺻"];
    )]
    #[test]
    fn test_name() {
        let result = quantize_redis_string(input);
        assert_eq!(result, expected);
    }

    #[duplicate_item(
    test_name input expected;
    [test_obfuscate_redis_string_1] ["AUTH my-secret-password"] ["AUTH ?"];
    [test_obfuscate_redis_string_2] ["AUTH james my-secret-password"] ["AUTH ?"];
    [test_obfuscate_redis_string_3] ["AUTH"] ["AUTH"];
    [test_obfuscate_redis_string_migrate_basic] ["MIGRATE host port key destination-db timeout"] ["MIGRATE ?"];
    [test_obfuscate_redis_string_migrate_with_flags] ["MIGRATE host port key destination-db timeout COPY REPLACE"] ["MIGRATE ?"];
    [test_obfuscate_redis_string_migrate_with_keys] [r#"MIGRATE host port "" destination-db timeout KEYS key1 key2 key3"#] ["MIGRATE ?"];
    [test_obfuscate_redis_string_migrate_no_args] ["MIGRATE"] ["MIGRATE"];
    [test_obfuscate_redis_string_hello_version] ["HELLO 3"] ["HELLO ?"];
    [test_obfuscate_redis_string_hello_auth] ["HELLO 3 AUTH username password"] ["HELLO ?"];
    [test_obfuscate_redis_string_hello_auth_setname] ["HELLO 3 AUTH username password SETNAME clientname"] ["HELLO ?"];
    [test_obfuscate_redis_string_hello_no_args] ["HELLO"] ["HELLO"];
    [test_obfuscate_redis_string_acl_setuser] ["ACL SETUSER alice on >password ~* &* +@all"] ["ACL SETUSER ?"];
    [test_obfuscate_redis_string_acl_setuser_complex] ["ACL SETUSER bob on >mysecretpassword ~keys:* resetchannels &channel:* +@all -@dangerous"] ["ACL SETUSER ?"];
    [test_obfuscate_redis_string_acl_getuser] ["ACL GETUSER alice"] ["ACL GETUSER ?"];
    [test_obfuscate_redis_string_acl_deluser] ["ACL DELUSER alice"] ["ACL DELUSER ?"];
    [test_obfuscate_redis_string_acl_deluser_multi] ["ACL DELUSER alice bob charlie"] ["ACL DELUSER ?"];
    [test_obfuscate_redis_string_acl_list] ["ACL LIST"] ["ACL LIST"];
    [test_obfuscate_redis_string_acl_whoami] ["ACL WHOAMI"] ["ACL WHOAMI"];
    [test_obfuscate_redis_string_acl_no_args] ["ACL"] ["ACL"];
    [test_obfuscate_redis_string_4] ["APPEND key value"] ["APPEND key ?"];
    [test_obfuscate_redis_string_5] ["GETSET key value"] ["GETSET key ?"];
    [test_obfuscate_redis_string_6] ["LPUSHX key value"] ["LPUSHX key ?"];
    [test_obfuscate_redis_string_7] ["GEORADIUSBYMEMBER key member radius m|km|ft|mi [WITHCOORD] [WITHDIST] [WITHHASH] [COUNT count] [ASC|DESC] [STORE key] [STOREDIST key]"] ["GEORADIUSBYMEMBER key ? radius m|km|ft|mi [WITHCOORD] [WITHDIST] [WITHHASH] [COUNT count] [ASC|DESC] [STORE key] [STOREDIST key]"];
    [test_obfuscate_redis_string_8] ["RPUSHX key value"] ["RPUSHX key ?"];
    [test_obfuscate_redis_string_9] ["SET key value"] ["SET key ?"];
    [test_obfuscate_redis_string_10] ["SET key value [expiration EX seconds|PX milliseconds] [NX|XX]"] ["SET key ? [expiration EX seconds|PX milliseconds] [NX|XX]"];
    [test_obfuscate_redis_string_11] ["SETNX key value"] ["SETNX key ?"];
    [test_obfuscate_redis_string_12] ["SISMEMBER key member"] ["SISMEMBER key ?"];
    [test_obfuscate_redis_string_13] ["ZRANK key member"] ["ZRANK key ?"];
    [test_obfuscate_redis_string_14] ["ZREVRANK key member"] ["ZREVRANK key ?"];
    [test_obfuscate_redis_string_15] ["ZSCORE key member"] ["ZSCORE key ?"];
    [test_obfuscate_redis_string_16] ["BITFIELD key GET type offset SET type offset value INCRBY type"] ["BITFIELD key GET type offset SET type offset ? INCRBY type"];
    [test_obfuscate_redis_string_17] ["BITFIELD key SET type offset value INCRBY type"] ["BITFIELD key SET type offset ? INCRBY type"];
    [test_obfuscate_redis_string_18] ["BITFIELD key GET type offset INCRBY type"] ["BITFIELD key GET type offset INCRBY type"];
    [test_obfuscate_redis_string_19] ["BITFIELD key SET type offset"] ["BITFIELD key SET type offset"];
    [test_obfuscate_redis_string_20] ["CONFIG SET parameter value"] ["CONFIG SET parameter ?"];
    [test_obfuscate_redis_string_21] ["CONFIG foo bar baz"] ["CONFIG foo bar baz"];
    [test_obfuscate_redis_string_22] ["GEOADD key longitude latitude member longitude latitude member longitude latitude member"] ["GEOADD key longitude latitude ? longitude latitude ? longitude latitude ?"];
    [test_obfuscate_redis_string_23] ["GEOADD key longitude latitude member longitude latitude member"] ["GEOADD key longitude latitude ? longitude latitude ?"];
    [test_obfuscate_redis_string_24] ["GEOADD key longitude latitude member"] ["GEOADD key longitude latitude ?"];
    [test_obfuscate_redis_string_25] ["GEOADD key longitude latitude"] ["GEOADD key longitude latitude"];
    [test_obfuscate_redis_string_26] ["GEOADD key"] ["GEOADD key"];
    [test_obfuscate_redis_string_27] ["GEOHASH key\nGEOPOS key\n GEODIST key"] ["GEOHASH key\nGEOPOS key\nGEODIST key"];
    [test_obfuscate_redis_string_28] ["GEOHASH key member\nGEOPOS key member\nGEODIST key member\n"] ["GEOHASH key ?\nGEOPOS key ?\nGEODIST key ?"];
    [test_obfuscate_redis_string_29] ["GEOHASH key member member member\nGEOPOS key member member \n  GEODIST key member member member"] ["GEOHASH key ?\nGEOPOS key ?\nGEODIST key ?"];
    [test_obfuscate_redis_string_30] ["GEOPOS key member [member ...]"] ["GEOPOS key ?"];
    [test_obfuscate_redis_string_31] ["SREM key member [member ...]"] ["SREM key ?"];
    [test_obfuscate_redis_string_32] ["ZREM key member [member ...]"] ["ZREM key ?"];
    [test_obfuscate_redis_string_33] ["SADD key member [member ...]"] ["SADD key ?"];
    [test_obfuscate_redis_string_34] ["GEODIST key member1 member2 [unit]"] ["GEODIST key ?"];
    [test_obfuscate_redis_string_35] ["LPUSH key value [value ...]"] ["LPUSH key ?"];
    [test_obfuscate_redis_string_36] ["RPUSH key value [value ...]"] ["RPUSH key ?"];
    [test_obfuscate_redis_string_37] ["HSET key field value \nHSETNX key field value\nBLAH"] ["HSET key field ?\nHSETNX key field ?\nBLAH"];
    [test_obfuscate_redis_string_38] ["HSET key field value"] ["HSET key field ?"];
    [test_obfuscate_redis_string_39] ["HSETNX key field value"] ["HSETNX key field ?"];
    [test_obfuscate_redis_string_40] ["LREM key count value"] ["LREM key count ?"];
    [test_obfuscate_redis_string_41] ["LSET key index value"] ["LSET key index ?"];
    [test_obfuscate_redis_string_42] ["SETBIT key offset value"] ["SETBIT key offset ?"];
    [test_obfuscate_redis_string_43] ["SETRANGE key offset value"] ["SETRANGE key offset ?"];
    [test_obfuscate_redis_string_44] ["SETEX key seconds value"] ["SETEX key seconds ?"];
    [test_obfuscate_redis_string_45] ["PSETEX key milliseconds value"] ["PSETEX key milliseconds ?"];
    [test_obfuscate_redis_string_46] ["ZINCRBY key increment member"] ["ZINCRBY key increment ?"];
    [test_obfuscate_redis_string_47] ["SMOVE source destination member"] ["SMOVE source destination ?"];
    [test_obfuscate_redis_string_48] ["RESTORE key ttl serialized-value [REPLACE]"] ["RESTORE key ttl ? [REPLACE]"];
    [test_obfuscate_redis_string_49] ["LINSERT key BEFORE pivot value"] ["LINSERT key BEFORE pivot ?"];
    [test_obfuscate_redis_string_50] ["LINSERT key AFTER pivot value"] ["LINSERT key AFTER pivot ?"];
    [test_obfuscate_redis_string_51] ["HMSET key field value field value"] ["HMSET key field ? field ?"];
    [test_obfuscate_redis_string_52] ["HMSET key field value \n HMSET key field value\n\n "] ["HMSET key field ?\nHMSET key field ?"];
    [test_obfuscate_redis_string_53] ["HMSET key field"] ["HMSET key field"];
    [test_obfuscate_redis_string_54] ["MSET key value key value"] ["MSET key ? key ?"];
    [test_obfuscate_redis_string_55] ["HMSET key field"] ["HMSET key field"];
    [test_obfuscate_redis_string_56] ["MSET\nMSET key value"] ["MSET\nMSET key ?"];
    [test_obfuscate_redis_string_57] ["MSET key value"] ["MSET key ?"];
    [test_obfuscate_redis_string_58] ["MSETNX key value key value"] ["MSETNX key ? key ?"];
    [test_obfuscate_redis_string_59] ["ZADD key score member score member"] ["ZADD key score ? score ?"];
    [test_obfuscate_redis_string_60] ["ZADD key NX score member score member"] ["ZADD key NX score ? score ?"];
    [test_obfuscate_redis_string_61] ["ZADD key NX CH score member score member"] ["ZADD key NX CH score ? score ?"];
    [test_obfuscate_redis_string_62] ["ZADD key NX CH INCR score member score member"] ["ZADD key NX CH INCR score ? score ?"];
    [test_obfuscate_redis_string_63] ["ZADD key XX INCR score member score member"] ["ZADD key XX INCR score ? score ?"];
    [test_obfuscate_redis_string_64] ["ZADD key XX INCR score member"] ["ZADD key XX INCR score ?"];
    [test_obfuscate_redis_string_65] ["ZADD key XX INCR score"] ["ZADD key XX INCR score"];
    [test_obfuscate_redis_string_66] [r"
CONFIG command
SET k v
                        "] [r"CONFIG command
SET k ?"];
    [test_obfuscate_redis_string_67] ["HSET key field value field value"] ["HSET key field ? field ?"];
    )]
    #[test]
    fn test_name() {
        let result = obfuscate_redis_string(input);
        assert_eq!(result, expected);
    }

    #[duplicate_item(
    test_name input expected;
    [test_obfuscate_all_redis_args_1] [""] [""];
    [test_obfuscate_all_redis_args_2] ["SET key value"] ["SET ?"];
    [test_obfuscate_all_redis_args_3] ["GET k"] ["GET ?"];
    [test_obfuscate_all_redis_args_4] ["FAKECMD key value hash"] ["FAKECMD ?"];
    [test_obfuscate_all_redis_args_5] ["AUTH password"] ["AUTH ?"];
    [test_obfuscate_all_redis_args_6] ["GET"] ["GET"];
    [test_obfuscate_all_redis_args_7] ["CONFIG SET key value"] ["CONFIG SET ?"];
    [test_obfuscate_all_redis_args_8] ["CONFIG GET key"] ["CONFIG GET ?"];
    [test_obfuscate_all_redis_args_9] ["CONFIG key"] ["CONFIG ?"];
    [test_obfuscate_all_redis_args_10] ["BITFIELD key SET key value GET key"] ["BITFIELD ? SET ? GET ?"];
    [test_obfuscate_all_redis_args_11] ["BITFIELD key INCRBY value"] ["BITFIELD ? INCRBY ?"];
    [test_obfuscate_all_redis_args_12] ["BITFIELD secret key"] ["BITFIELD ?"];
    [test_obfuscate_all_redis_args_13] ["set key value"] ["set ?"];
    [test_obfuscate_all_redis_args_14] ["Get key"] ["Get ?"];
    [test_obfuscate_all_redis_args_15] ["config key"] ["config ?"];
    [test_obfuscate_all_redis_args_16] ["CONFIG get key"] ["CONFIG get ?"];
    [test_obfuscate_all_redis_args_17] ["bitfield key SET key value incrby 3"] ["bitfield ? SET ? incrby ?"];
    [test_obfuscate_fuzzing_unicode] ["\u{00b}ჸ"] ["ჸ"];
    [test_obfuscate_fuzzing_whitespaces] ["ჸ\n\tჸ"] ["ჸ ?"];
    )]
    #[test]
    fn test_name() {
        let result = remove_all_redis_args(input);
        assert_eq!(result, expected);
    }
}
