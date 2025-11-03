// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{black_box, criterion_group, Criterion};
use libdd_trace_obfuscation::redis;

fn obfuscate_redis_string_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("redis");
    let cases = [
        "AUTH my-secret-password",
        "AUTH james my-secret-password",
        "AUTH",
        "APPEND key value",
        "GETSET key value",
        "LPUSHX key value",
        "GEORADIUSBYMEMBER key member radius m|km|ft|mi [WITHCOORD] [WITHDIST] [WITHHASH] [COUNT count] [ASC|DESC] [STORE key] [STOREDIST key]",
        "RPUSHX key value",
        "SET key value",
        "SET key value [expiration EX seconds|PX milliseconds] [NX|XX]",
        "SETNX key value",
        "SISMEMBER key member",
        "ZRANK key member",
        "ZREVRANK key member",
        "ZSCORE key member",
        "BITFIELD key GET type offset SET type offset value INCRBY type",
        "BITFIELD key SET type offset value INCRBY type",
        "BITFIELD key GET type offset INCRBY type",
        "BITFIELD key SET type offset",
        "CONFIG SET parameter value",
        "CONFIG foo bar baz",
        "GEOADD key longitude latitude member longitude latitude member longitude latitude member",
        "GEOADD key longitude latitude member longitude latitude member",
        "GEOADD key longitude latitude member",
        "GEOADD key longitude latitude",
        "GEOADD key",
        "GEOHASH key\nGEOPOS key\n GEODIST key",
        "GEOHASH key member\nGEOPOS key member\nGEODIST key member\n",
        "GEOHASH key member member member\nGEOPOS key member member \n  GEODIST key member member member",
        "GEOPOS key member [member ...]",
        "SREM key member [member ...]",
        "ZREM key member [member ...]",
        "SADD key member [member ...]",
        "GEODIST key member1 member2 [unit]",
        "LPUSH key value [value ...]",
        "RPUSH key value [value ...]",
        "HSET key field value \nHSETNX key field value\nBLAH",
        "HSET key field value",
        "HSETNX key field value",
        "LREM key count value",
        "LSET key index value",
        "SETBIT key offset value",
        "SETRANGE key offset value",
        "SETEX key seconds value",
        "PSETEX key milliseconds value",
        "ZINCRBY key increment member",
        "SMOVE source destination member",
        "RESTORE key ttl serialized-value [REPLACE]",
        "LINSERT key BEFORE pivot value",
        "LINSERT key AFTER pivot value",
        "HMSET key field value field value",
        "HMSET key field value \n HMSET key field value\n\n ",
        "HMSET key field",
        "MSET key value key value",
        "HMSET key field",
        "MSET\nMSET key value",
        "MSET key value",
        "MSETNX key value key value",
        "ZADD key score member score member",
        "ZADD key NX score member score member",
        "ZADD key NX CH score member score member",
        "ZADD key NX CH INCR score member score member",
        "ZADD key XX INCR score member score member",
        "ZADD key XX INCR score member",
        "ZADD key XX INCR score",
        r#"
CONFIG command
SET k v
                    "#,
        "",
        "SET key value",
        "GET k",
        "FAKECMD key value hash",
        "AUTH password",
        "GET",
        "CONFIG SET key value",
        "CONFIG GET key",
        "CONFIG key",
        "BITFIELD key SET key value GET key",
        "BITFIELD key INCRBY value",
        "BITFIELD secret key",
        "set key value",
        "Get key",
        "config key",
        "CONFIG get key",
        "bitfield key SET key value incrby 3",
    ];

    group.bench_function("obfuscate_redis_string", |b| {
        b.iter_batched_ref(
            // Keep the String instances around to avoid measuring the deallocation cost
            || Vec::with_capacity(cases.len()) as Vec<String>,
            |res: &mut Vec<String>| {
                for c in cases {
                    res.push(black_box(redis::obfuscate_redis_string(c)));
                }
            },
            criterion::BatchSize::LargeInput,
        )
    });
}

criterion_group!(benches, obfuscate_redis_string_benchmark);
