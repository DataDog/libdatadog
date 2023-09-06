// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use crate::redis_tokenizer::{RedisTokenType, RedisTokenizer};

pub fn obfuscate_redis_string(cmd: &str) -> String {
    let mut tokenizer = RedisTokenizer::new(cmd);
    let s = &mut String::new();
    let mut cmd: Option<String> = None;
    let mut args: Vec<String> = Vec::new();

    loop {
        let res = tokenizer.scan();
        match res.token_type {
            RedisTokenType::RedisTokenCommand => {
                if let Some(cmd) = cmd {
                    args = obfuscate_redis_cmd(s, cmd, args);
                    s.push('\n');
                }
                cmd = Some(res.token);
                args.clear();
            }
            RedisTokenType::RedisTokenArgument => args.push(res.token),
        }
        if res.done {
            obfuscate_redis_cmd(s, cmd.unwrap_or_default(), args);
            break;
        }
    }
    s.to_string()
}

fn obfuscate_redis_cmd(str: &mut String, cmd: String, mut args: Vec<String>) -> Vec<String> {
    str.push_str(&cmd);
    if args.is_empty() {
        return args;
    }
    str.push(' ');
    match cmd.to_uppercase().as_str() {
        "AUTH" => {
            if !args.is_empty() {
                args.clear();
                args.push("?".to_string());
            }
        }
        "APPEND" | "GETSET" | "LPUSHX" | "GEORADIUSBYMEMBER" | "RPUSHX" | "SET" | "SETNX"
        | "SISMEMBER" | "ZRANK" | "ZREVRANK" | "ZSCORE" => {
            // Obfuscate 2nd argument:
            // • APPEND key value
            // • GETSET key value
            // • LPUSHX key value
            // • GEORADIUSBYMEMBER key member radius m|km|ft|mi [WITHCOORD] [WITHDIST] [WITHHASH] [COUNT count] [ASC|DESC] [STORE key] [STOREDIST key]
            // • RPUSHX key value
            // • SET key value [expiration EX seconds|PX milliseconds] [NX|XX]
            // • SETNX key value
            // • SISMEMBER key member
            // • ZRANK key member
            // • ZREVRANK key member
            // • ZSCORE key member
            args = obfuscate_redis_args_n(args, 1);
        }
        "HSET" | "HSETNX" | "LREM" | "LSET" | "SETBIT" | "SETEX" | "PSETEX" | "SETRANGE"
        | "ZINCRBY" | "SMOVE" | "RESTORE" => {
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
        "LINSERT" => {
            // Obfuscate 4th argument:
            // • LINSERT key BEFORE|AFTER pivot value
            args = obfuscate_redis_args_n(args, 3);
        }
        "GEOHASH" | "GEOPOS" | "GEODIST" | "LPUSH" | "RPUSH" | "SREM" | "ZREM" | "SADD" => {
            // Obfuscate all arguments after the first one.
            // • GEOHASH key member [member ...]
            // • GEOPOS key member [member ...]
            // • GEODIST key member1 member2 [unit]
            // • LPUSH key value [value ...]
            // • RPUSH key value [value ...]
            // • SREM key member [member ...]
            // • ZREM key member [member ...]
            // • SADD key member [member ...]
            if args.len() > 1 {
                args[1] = "?".to_string();
                args.drain(2..);
            }
        }
        "GEOADD" => {
            // Obfuscating every 3rd argument starting from first
            // • GEOADD key longitude latitude member [longitude latitude member ...]
            args = obfuscate_redis_args_step(args, 1, 3)
        }
        "HMSET" => {
            // Every 2nd argument starting from first.
            // • HMSET key field value [field value ...]
            args = obfuscate_redis_args_step(args, 1, 2)
        }
        "MSET" | "MSETNX" => {
            // Every 2nd argument starting from command.
            // • MSET key value [key value ...]
            // • MSETNX key value [key value ...]
            args = obfuscate_redis_args_step(args, 0, 2)
        }
        "CONFIG" => {
            // Obfuscate 2nd argument to SET sub-command.
            // • CONFIG SET parameter value
            if args[0].to_uppercase() == "SET" {
                args = obfuscate_redis_args_n(args, 2)
            }
        }
        "BITFIELD" => {
            // Obfuscate 3rd argument to SET sub-command:
            // • BITFIELD key [GET type offset] [SET type offset value] [INCRBY type offset increment] [OVERFLOW WRAP|SAT|FAIL]
            let mut n = 0;
            for (i, arg) in args.iter_mut().enumerate() {
                if arg.to_uppercase() == "SET" {
                    n = i;
                }
                if n > 0 && i - n == 3 {
                    *arg = "?".to_string();
                    break;
                }
            }
        }
        "ZADD" => {
            for i in 0..args.len() {
                if i == 0 {
                    continue; // key
                }
                match args[i].as_str() {
                    "NX" | "XX" | "CH" | "INCR" => {}
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

fn obfuscate_redis_args_n(mut args: Vec<String>, n: usize) -> Vec<String> {
    if args.len() > n {
        args[n] = "?".to_string();
    }
    args
}

fn obfuscate_redis_args_step(mut args: Vec<String>, start: usize, step: usize) -> Vec<String> {
    if start + step > args.len() {
        return args;
    }
    for i in ((start + step - 1)..args.len()).step_by(step) {
        args[i] = "?".to_string()
    }
    args
}

pub fn remove_all_redis_args(redis_cmd: &str) -> String {
    let mut redis_cmd_iter = redis_cmd.split_whitespace().peekable();
    let mut obfuscated_cmd = String::new();
    if redis_cmd_iter.peek().is_none() {
        return obfuscated_cmd;
    }

    let cmd = redis_cmd_iter.next().unwrap_or_default();
    obfuscated_cmd.push_str(cmd);

    if redis_cmd_iter.peek().is_none() {
        return obfuscated_cmd;
    }

    obfuscated_cmd.push(' ');

    match cmd.to_uppercase().as_str() {
        "BITFIELD" => {
            obfuscated_cmd.push('?');
            for a in redis_cmd_iter {
                let arg = a.to_uppercase();
                if arg == "SET" || arg == "GET" || arg == "INCRBY" {
                    obfuscated_cmd.push_str(format!(" {a} ?").as_str());
                }
            }
        }
        "CONFIG" => {
            let a = redis_cmd_iter.next().unwrap_or_default();
            let arg = a.to_uppercase();
            if arg == "GET" || arg == "SET" || arg == "RESETSTAT" || arg == "REWRITE" {
                // out.WriteString(strings.Join([]string{args[0], "?"}, " "))
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

#[cfg(test)]
mod tests {
    use duplicate::duplicate_item;

    use super::{obfuscate_redis_string, remove_all_redis_args};

    #[duplicate_item(
        [
            test_name   [test_obfuscate_redis_string_1]
            input       ["AUTH my-secret-password"]
            expected    ["AUTH ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_2]
            input       ["AUTH james my-secret-password"]
            expected    ["AUTH ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_3]
            input       ["AUTH"]
            expected    ["AUTH"];
        ]
        [
            test_name   [test_obfuscate_redis_string_4]
            input       ["APPEND key value"]
            expected    ["APPEND key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_5]
            input       ["GETSET key value"]
            expected    ["GETSET key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_6]
            input       ["LPUSHX key value"]
            expected    ["LPUSHX key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_7]
            input       ["GEORADIUSBYMEMBER key member radius m|km|ft|mi [WITHCOORD] [WITHDIST] [WITHHASH] [COUNT count] [ASC|DESC] [STORE key] [STOREDIST key]"]
            expected    ["GEORADIUSBYMEMBER key ? radius m|km|ft|mi [WITHCOORD] [WITHDIST] [WITHHASH] [COUNT count] [ASC|DESC] [STORE key] [STOREDIST key]"];
        ]
        [
            test_name   [test_obfuscate_redis_string_8]
            input       ["RPUSHX key value"]
            expected    ["RPUSHX key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_9]
            input       ["SET key value"]
            expected    ["SET key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_10]
            input       ["SET key value [expiration EX seconds|PX milliseconds] [NX|XX]"]
            expected    ["SET key ? [expiration EX seconds|PX milliseconds] [NX|XX]"];
        ]
        [
            test_name   [test_obfuscate_redis_string_11]
            input       ["SETNX key value"]
            expected    ["SETNX key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_12]
            input       ["SISMEMBER key member"]
            expected    ["SISMEMBER key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_13]
            input       ["ZRANK key member"]
            expected    ["ZRANK key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_14]
            input       ["ZREVRANK key member"]
            expected    ["ZREVRANK key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_15]
            input       ["ZSCORE key member"]
            expected    ["ZSCORE key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_16]
            input       ["BITFIELD key GET type offset SET type offset value INCRBY type"]
            expected    ["BITFIELD key GET type offset SET type offset ? INCRBY type"];
        ]
        [
            test_name   [test_obfuscate_redis_string_17]
            input       ["BITFIELD key SET type offset value INCRBY type"]
            expected    ["BITFIELD key SET type offset ? INCRBY type"];
        ]
        [
            test_name   [test_obfuscate_redis_string_18]
            input       ["BITFIELD key GET type offset INCRBY type"]
            expected    ["BITFIELD key GET type offset INCRBY type"];
        ]
        [
            test_name   [test_obfuscate_redis_string_19]
            input       ["BITFIELD key SET type offset"]
            expected    ["BITFIELD key SET type offset"];
        ]
        [
            test_name   [test_obfuscate_redis_string_20]
            input       ["CONFIG SET parameter value"]
            expected    ["CONFIG SET parameter ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_21]
            input       ["CONFIG foo bar baz"]
            expected    ["CONFIG foo bar baz"];
        ]
        [
            test_name   [test_obfuscate_redis_string_22]
            input       ["GEOADD key longitude latitude member longitude latitude member longitude latitude member"]
            expected    ["GEOADD key longitude latitude ? longitude latitude ? longitude latitude ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_23]
            input       ["GEOADD key longitude latitude member longitude latitude member"]
            expected    ["GEOADD key longitude latitude ? longitude latitude ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_24]
            input       ["GEOADD key longitude latitude member"]
            expected    ["GEOADD key longitude latitude ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_25]
            input       ["GEOADD key longitude latitude"]
            expected    ["GEOADD key longitude latitude"];
        ]
        [
            test_name   [test_obfuscate_redis_string_26]
            input       ["GEOADD key"]
            expected    ["GEOADD key"];
        ]
        [
            test_name   [test_obfuscate_redis_string_27]
            input       ["GEOHASH key\nGEOPOS key\n GEODIST key"]
            expected    ["GEOHASH key\nGEOPOS key\nGEODIST key"];
        ]
        [
            test_name   [test_obfuscate_redis_string_28]
            input       ["GEOHASH key member\nGEOPOS key member\nGEODIST key member\n"]
            expected    ["GEOHASH key ?\nGEOPOS key ?\nGEODIST key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_29]
            input       ["GEOHASH key member member member\nGEOPOS key member member \n  GEODIST key member member member"]
            expected    ["GEOHASH key ?\nGEOPOS key ?\nGEODIST key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_30]
            input       ["GEOPOS key member [member ...]"]
            expected    ["GEOPOS key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_31]
            input       ["SREM key member [member ...]"]
            expected    ["SREM key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_32]
            input       ["ZREM key member [member ...]"]
            expected    ["ZREM key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_33]
            input       ["SADD key member [member ...]"]
            expected    ["SADD key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_34]
            input       ["GEODIST key member1 member2 [unit]"]
            expected    ["GEODIST key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_35]
            input       ["LPUSH key value [value ...]"]
            expected    ["LPUSH key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_36]
            input       ["RPUSH key value [value ...]"]
            expected    ["RPUSH key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_37]
            input       ["HSET key field value \nHSETNX key field value\nBLAH"]
            expected    ["HSET key field ?\nHSETNX key field ?\nBLAH"];
        ]
        [
            test_name   [test_obfuscate_redis_string_38]
            input       ["HSET key field value"]
            expected    ["HSET key field ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_39]
            input       ["HSETNX key field value"]
            expected    ["HSETNX key field ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_40]
            input       ["LREM key count value"]
            expected    ["LREM key count ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_41]
            input       ["LSET key index value"]
            expected    ["LSET key index ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_42]
            input       ["SETBIT key offset value"]
            expected    ["SETBIT key offset ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_43]
            input       ["SETRANGE key offset value"]
            expected    ["SETRANGE key offset ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_44]
            input       ["SETEX key seconds value"]
            expected    ["SETEX key seconds ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_45]
            input       ["PSETEX key milliseconds value"]
            expected    ["PSETEX key milliseconds ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_46]
            input       ["ZINCRBY key increment member"]
            expected    ["ZINCRBY key increment ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_47]
            input       ["SMOVE source destination member"]
            expected    ["SMOVE source destination ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_48]
            input       ["RESTORE key ttl serialized-value [REPLACE]"]
            expected    ["RESTORE key ttl ? [REPLACE]"];
        ]
        [
            test_name   [test_obfuscate_redis_string_49]
            input       ["LINSERT key BEFORE pivot value"]
            expected    ["LINSERT key BEFORE pivot ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_50]
            input       ["LINSERT key AFTER pivot value"]
            expected    ["LINSERT key AFTER pivot ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_51]
            input       ["HMSET key field value field value"]
            expected    ["HMSET key field ? field ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_52]
            input       ["HMSET key field value \n HMSET key field value\n\n "]
            expected    ["HMSET key field ?\nHMSET key field ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_53]
            input       ["HMSET key field"]
            expected    ["HMSET key field"];
        ]
        [
            test_name   [test_obfuscate_redis_string_54]
            input       ["MSET key value key value"]
            expected    ["MSET key ? key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_55]
            input       ["HMSET key field"]
            expected    ["HMSET key field"];
        ]
        [
            test_name   [test_obfuscate_redis_string_56]
            input       ["MSET\nMSET key value"]
            expected    ["MSET\nMSET key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_57]
            input       ["MSET key value"]
            expected    ["MSET key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_58]
            input       ["MSETNX key value key value"]
            expected    ["MSETNX key ? key ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_59]
            input       ["ZADD key score member score member"]
            expected    ["ZADD key score ? score ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_60]
            input       ["ZADD key NX score member score member"]
            expected    ["ZADD key NX score ? score ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_61]
            input       ["ZADD key NX CH score member score member"]
            expected    ["ZADD key NX CH score ? score ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_62]
            input       ["ZADD key NX CH INCR score member score member"]
            expected    ["ZADD key NX CH INCR score ? score ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_63]
            input       ["ZADD key XX INCR score member score member"]
            expected    ["ZADD key XX INCR score ? score ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_64]
            input       ["ZADD key XX INCR score member"]
            expected    ["ZADD key XX INCR score ?"];
        ]
        [
            test_name   [test_obfuscate_redis_string_65]
            input       ["ZADD key XX INCR score"]
            expected    ["ZADD key XX INCR score"];
        ]
        [
            test_name   [test_obfuscate_redis_string_66]
            input       [r#"
CONFIG command
SET k v
                        "#]
            expected    [r#"CONFIG command
SET k ?"#];
        ]
    )]
    #[test]
    fn test_name() {
        let result = obfuscate_redis_string(input);
        assert_eq!(result, expected);
    }

    #[duplicate_item(
        [
            test_name   [test_obfuscate_all_redis_args_1]
            input       [""]
            expected    [""];
        ]
        [
            test_name   [test_obfuscate_all_redis_args_2]
            input       ["SET key value"]
            expected    ["SET ?"];
        ]
        [
            test_name   [test_obfuscate_all_redis_args_3]
            input       ["GET k"]
            expected    ["GET ?"];
        ]
        [
            test_name   [test_obfuscate_all_redis_args_4]
            input       ["FAKECMD key value hash"]
            expected    ["FAKECMD ?"];
        ]
        [
            test_name   [test_obfuscate_all_redis_args_5]
            input       ["AUTH password"]
            expected    ["AUTH ?"];
        ]
        [
            test_name   [test_obfuscate_all_redis_args_6]
            input       ["GET"]
            expected    ["GET"];
        ]
        [
            test_name   [test_obfuscate_all_redis_args_7]
            input       ["CONFIG SET key value"]
            expected    ["CONFIG SET ?"];
        ]
        [
            test_name   [test_obfuscate_all_redis_args_8]
            input       ["CONFIG GET key"]
            expected    ["CONFIG GET ?"];
        ]
        [
            test_name   [test_obfuscate_all_redis_args_9]
            input       ["CONFIG key"]
            expected    ["CONFIG ?"];
        ]
        [
            test_name   [test_obfuscate_all_redis_args_10]
            input       ["BITFIELD key SET key value GET key"]
            expected    ["BITFIELD ? SET ? GET ?"];
        ]
        [
            test_name   [test_obfuscate_all_redis_args_11]
            input       ["BITFIELD key INCRBY value"]
            expected    ["BITFIELD ? INCRBY ?"];
        ]
        [
            test_name   [test_obfuscate_all_redis_args_12]
            input       ["BITFIELD secret key"]
            expected    ["BITFIELD ?"];
        ]
        [
            test_name   [test_obfuscate_all_redis_args_13]
            input       ["set key value"]
            expected    ["set ?"];
        ]
        [
            test_name   [test_obfuscate_all_redis_args_14]
            input       ["Get key"]
            expected    ["Get ?"];
        ]
        [
            test_name   [test_obfuscate_all_redis_args_15]
            input       ["config key"]
            expected    ["config ?"];
        ]
        [
            test_name   [test_obfuscate_all_redis_args_16]
            input       ["CONFIG get key"]
            expected    ["CONFIG get ?"];
        ]
        [
            test_name   [test_obfuscate_all_redis_args_17]
            input       ["bitfield key SET key value incrby 3"]
            expected    ["bitfield ? SET ? incrby ?"];
        ]
    )]
    #[test]
    fn test_name() {
        let result = remove_all_redis_args(input);
        assert_eq!(result, expected);
    }
}
