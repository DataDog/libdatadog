// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

use std::collections::HashSet;

use serde_json::{json, Value};

use crate::{obfuscation_config::ObfuscationConfig, sql::obfuscate_sql_string};

pub enum JSONObfuscationType {
    MongoDB,
    Elasticsearch,
}

pub fn obfuscate_json_string(
    config: &ObfuscationConfig,
    obfuscation_type: JSONObfuscationType,
    json_str: &str,
) -> String {
    let mut json_dict: Value = serde_json::from_str(json_str).unwrap_or_default();
    if json_dict.is_null() {
        return "?".to_string();
    }

    let empty_vec = Vec::new();

    let json_keep_values = match obfuscation_type {
        JSONObfuscationType::MongoDB => config.mongodb_keep_values.as_ref(),
        JSONObfuscationType::Elasticsearch => config.elasticsearch_keep_values.as_ref(),
    }
    .unwrap_or(&empty_vec);

    let json_sql_values = match obfuscation_type {
        JSONObfuscationType::MongoDB => config.mongodb_obfuscate_sql_values.as_ref(),
        JSONObfuscationType::Elasticsearch => config.elasticsearch_obfuscate_sql_values.as_ref(),
    }
    .unwrap_or(&empty_vec);

    recurse_and_replace_json(
        config,
        &mut json_dict,
        &HashSet::from_iter(json_keep_values.iter().map(|s| s.to_string())),
        &HashSet::from_iter(json_sql_values.iter().map(|s| s.to_string())),
    );

    json_dict.to_string()
}

fn recurse_and_replace_json(
    config: &ObfuscationConfig,
    value: &mut Value,
    keep_values: &HashSet<String>,
    sql_values: &HashSet<String>,
) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if keep_values.contains(k) {
                    continue;
                }
                if v.is_array() || v.is_object() {
                    recurse_and_replace_json(config, v, keep_values, sql_values);
                    continue;
                }

                if sql_values.contains(k) {
                    let obfuscated_sql_result =
                        obfuscate_sql_string(v.as_str().unwrap_or_default(), config);
                    if let Some(res) = obfuscated_sql_result.obfuscated_string {
                        *v = json!(res)
                    } else {
                        *v = json!("?");
                    }
                } else {
                    *v = json!("?");
                }
                recurse_and_replace_json(config, v, keep_values, sql_values);
            }
        }
        Value::Array(arr) => {
            for (_, v) in arr.iter_mut().enumerate() {
                if !v.is_array() && !v.is_object() {
                    *v = json!("?");
                }
                recurse_and_replace_json(config, v, keep_values, sql_values);
            }
        }
        _ => (),
    }
}

#[cfg(test)]
mod tests {
    use duplicate::duplicate_item;
    use serde_json::json;

    use crate::{json::JSONObfuscationType, obfuscation_config::ObfuscationConfig};

    use super::obfuscate_json_string;

    fn parse_test_args(argv: Vec<&str>) -> Vec<String> {
        argv.iter().map(|&s| s.to_string()).collect::<Vec<String>>()
    }

    #[duplicate_item(
        [
            test_name       [test_obfuscate_json_1]
            keep_values     [vec![]]
            input           [json!( { "query": { "multi_match" : { "query" : "guide", "fields" : ["_all", { "key": "value", "other": ["1", "2", {"k": "v"}] }, "2"] } } } )]
            expected        [json!( { "query": { "multi_match": { "query": "?", "fields" : ["?", { "key": "?", "other": ["?", "?", {"k": "?"}] }, "?"] } } } )];
        ]
        [
            test_name       [test_obfuscate_json_2]
            keep_values     [vec![]]
            input           [json!({
                "highlight": {
                  "pre_tags": [ "<em>" ],
                  "post_tags": [ "</em>" ],
                  "index": 1
                }
              })]
            expected        [json!({
                "highlight": {
                  "pre_tags": [ "?" ],
                  "post_tags": [ "?" ],
                  "index": "?"
                }
              })];
        ]
        [
            test_name       [test_obfuscate_json_3]
            keep_values     [vec!["other"]]
            input           [json!( { "query": { "multi_match" : { "query" : "guide", "fields" : ["_all", { "key": "value", "other": ["1", "2", {"k": "v"}] }, "2"] } } } )]
            expected        [json!( { "query": { "multi_match": { "query": "?", "fields" : ["?", { "key": "?", "other": ["1", "2", {"k": "v"}] }, "?"] } } } )];
        ]
        [
            test_name       [test_obfuscate_json_4]
            keep_values     [vec!["fields"]]
            input           [json!( {"fields" : ["_all", { "key": "value", "other": ["1", "2", {"k": "v"}] }, "2"]} )]
            expected        [json!( {"fields" : ["_all", { "key": "value", "other": ["1", "2", {"k": "v"}] }, "2"]} )];
        ]
        [
            test_name       [test_obfuscate_json_5]
            keep_values     [vec!["k"]]
            input           [json!( {"fields" : ["_all", { "key": "value", "other": ["1", "2", {"k": "v"}] }, "2"]} )]
            expected        [json!( {"fields" : ["?", { "key": "?", "other": ["?", "?", {"k": "v"}] }, "?"]} )];
        ]
        [
            test_name       [test_obfuscate_json_6]
            keep_values     [vec!["C"]]
            input           [json!( {"fields" : [{"A": 1, "B": {"C": 3}}, "2"]} )]
            expected        [json!( {"fields" : [{"A": "?", "B": {"C": 3}}, "?"]} )];
        ]
        [
            test_name       [test_obfuscate_json_7]
            keep_values     [vec![]]
            input           [json!( {
                "query": {
                   "match" : {
                      "title" : "in action"
                   }
                },
                "size": 2,
                "from": 0,
                "_source": [ "title", "summary", "publish_date" ],
                "highlight": {
                   "fields" : {
                      "title" : {}
                   }
                }
            } )]
            expected        [json!( {
                "query": {
                   "match" : {
                      "title" : "?"
                   }
                },
                "size": "?",
                "from": "?",
                "_source": [ "?", "?", "?" ],
                "highlight": {
                   "fields" : {
                      "title" : {}
                   }
                }
            } )];
        ]
        [
            test_name       [test_obfuscate_json_8]
            keep_values     [vec!["_source"]]
            input           [json!( {
                "query": {
                   "match" : {
                      "title" : "in action"
                   }
                },
                "size": 2,
                "from": 0,
                "_source": [ "title", "summary", "publish_date" ],
                "highlight": {
                   "fields" : {
                      "title" : {}
                   }
                }
            } )]
            expected        [json!( {
                "query": {
                   "match" : {
                      "title" : "?"
                   }
                },
                "size": "?",
                "from": "?",
                "_source": [ "title", "summary", "publish_date" ],
                "highlight": {
                   "fields" : {
                      "title" : {}
                   }
                }
            } )];
        ]
        [
            test_name       [test_obfuscate_json_9]
            keep_values     [vec!["query"]]
            input           [json!( {
                "query": {
                   "match" : {
                      "title" : "in action"
                   }
                },
                "size": 2,
                "from": 0,
                "_source": [ "title", "summary", "publish_date" ],
                "highlight": {
                   "fields" : {
                      "title" : {}
                   }
                }
            } )]
            expected        [json!( {
                "query": {
                   "match" : {
                      "title" : "in action"
                   }
                },
                "size": "?",
                "from": "?",
                "_source": [ "?", "?", "?" ],
                "highlight": {
                   "fields" : {
                      "title" : {}
                   }
                }
            } )];
        ]
        [
            test_name       [test_obfuscate_json_10]
            keep_values     [vec!["match"]]
            input           [json!( {
                "query": {
                   "match" : {
                      "title" : "in action"
                   }
                },
                "size": 2,
                "from": 0,
                "_source": [ "title", "summary", "publish_date" ],
                "highlight": {
                   "fields" : {
                      "title" : {}
                   }
                }
            } )]
            expected        [json!( {
                "query": {
                   "match" : {
                      "title" : "in action"
                   }
                },
                "size": "?",
                "from": "?",
                "_source": [ "?", "?", "?" ],
                "highlight": {
                   "fields" : {
                      "title" : {}
                   }
                }
            } )];
        ]
        [
            test_name       [test_obfuscate_json_11]
            keep_values     [vec!["hits"]]
            input           [json!( {
                "outer": {
                    "total": 2,
                    "max_score": 0.9105287,
                    "hits": [
                     {
                       "_index": "bookdb_index",
                       "_type": "book",
                       "_id": "3",
                       "_score": 0.9105287,
                       "_source": {
                        "summary": "build scalable search applications using Elasticsearch without having to do complex low-level programming or understand advanced data science algorithms",
                        "title": "Elasticsearch in Action",
                        "publish_date": "2015-12-03"
                       },
                       "highlight": {
                        "title": [
                          "Elasticsearch Action"
                        ]
                       }
                     },
                     {
                       "_index": "bookdb_index",
                       "_type": "book",
                       "_id": "4",
                       "_score": 0.9105287,
                       "_source": {
                        "summary": "Comprehensive guide to implementing a scalable search engine using Apache Solr",
                        "title": "Solr in Action",
                        "publish_date": "2014-04-05"
                       },
                       "highlight": {
                        "title": [
                          "Solr in Action"
                        ]
                       }
                     }
                    ]
                }
            } )]
            expected        [json!( {
                "outer": {
                    "total": "?",
                    "max_score": "?",
                    "hits": [
                     {
                       "_index": "bookdb_index",
                       "_type": "book",
                       "_id": "3",
                       "_score": 0.9105287,
                       "_source": {
                        "summary": "build scalable search applications using Elasticsearch without having to do complex low-level programming or understand advanced data science algorithms",
                        "title": "Elasticsearch in Action",
                        "publish_date": "2015-12-03"
                       },
                       "highlight": {
                        "title": [
                          "Elasticsearch Action"
                        ]
                       }
                     },
                     {
                       "_index": "bookdb_index",
                       "_type": "book",
                       "_id": "4",
                       "_score": 0.9105287,
                       "_source": {
                        "summary": "Comprehensive guide to implementing a scalable search engine using Apache Solr",
                        "title": "Solr in Action",
                        "publish_date": "2014-04-05"
                       },
                       "highlight": {
                        "title": [
                          "Solr in Action"
                        ]
                       }
                     }
                    ]
                }
            } )];
        ]
        [
            test_name       [test_obfuscate_json_12]
            keep_values     [vec!["_index", "title"]]
            input           [json!( {
                "hits": {
                    "total": 2,
                    "max_score": 0.9105287,
                    "hits": [
                     {
                       "_index": "bookdb_index",
                       "_type": "book",
                       "_id": "3",
                       "_score": 0.9105287,
                       "_source": {
                        "summary": "build scalable search applications using Elasticsearch without having to do complex low-level programming or understand advanced data science algorithms",
                        "title": "Elasticsearch in Action",
                        "publish_date": "2015-12-03"
                       },
                       "highlight": {
                        "title": [
                          "Elasticsearch Action"
                        ]
                       }
                     },
                     {
                       "_index": "bookdb_index",
                       "_type": "book",
                       "_id": "4",
                       "_score": 0.9105287,
                       "_source": {
                        "summary": "Comprehensive guide to implementing a scalable search engine using Apache Solr",
                        "title": "Solr in Action",
                        "publish_date": "2014-04-05"
                       },
                       "highlight": {
                        "title": [
                          "Solr Action"
                        ]
                       }
                     }
                    ]
                }
            } )]
            expected        [json!( {
                "hits": {
                    "total": "?",
                    "max_score": "?",
                    "hits": [
                     {
                       "_index": "bookdb_index",
                       "_type": "?",
                       "_id": "?",
                       "_score": "?",
                       "_source": {
                        "summary": "?",
                        "title": "Elasticsearch in Action",
                        "publish_date": "?"
                       },
                       "highlight": {
                        "title": [
                          "Elasticsearch Action"
                        ]
                       }
                     },
                     {
                       "_index": "bookdb_index",
                       "_type": "?",
                       "_id": "?",
                       "_score": "?",
                       "_source": {
                        "summary": "?",
                        "title": "Solr in Action",
                        "publish_date": "?"
                       },
                       "highlight": {
                        "title": [
                          "Solr Action"
                        ]
                       }
                     }
                    ]
                }
            } )];
        ]
        [
            test_name       [test_obfuscate_json_13]
            keep_values     [vec!["_source"]]
            input           [json!( {
                "query": {
                  "bool": {
                    "must": [ { "match": { "title": "smith" } } ],
                    "must_not": [ { "match_phrase": { "title": "granny smith" } } ],
                    "filter": [ { "exists": { "field": "title" } } ]
                  }
                },
                "aggs": {
                  "my_agg": { "terms": { "field": "user", "size": 10 } }
                },
                "highlight": {
                  "pre_tags": [ "<em>" ], "post_tags": [ "</em>" ],
                  "fields": {
                    "body": { "number_of_fragments": 1, "fragment_size": 20 },
                    "title": {}
                  }
                },
                "size": 20,
                "from": 100,
                "_source": [ "title", "id" ],
                "sort": [ { "_id": { "order": "desc" } } ]
              } )]
            expected        [json!( {
                "query": {
                  "bool": {
                    "must": [ { "match": { "title": "?" } } ],
                    "must_not": [ { "match_phrase": { "title": "?" } } ],
                    "filter": [ { "exists": { "field": "?" } } ]
                  }
                },
                "aggs": {
                  "my_agg": { "terms": { "field": "?", "size": "?" } }
                },
                "highlight": {
                  "pre_tags": [ "?" ], "post_tags": [ "?" ],
                  "fields": {
                    "body": { "number_of_fragments": "?", "fragment_size": "?" },
                    "title": {}
                  }
                },
                "size": "?",
                "from": "?",
                "_source": [ "title", "id" ],
                "sort": [ { "_id": { "order": "?" } }
                ]
              } )];
        ]
    )]
    #[test]
    fn test_name() {
        let mut config = ObfuscationConfig::new_test_config();
        config.elasticsearch_keep_values = Some(parse_test_args(keep_values));
        let result = obfuscate_json_string(
            &config,
            JSONObfuscationType::Elasticsearch,
            input.to_string().as_str(),
        );
        assert_eq!(result, expected.to_string());
    }

    #[duplicate_item(
        [
            test_name       [test_obfuscate_json_sql_queries_1]
            keep_values     [vec!["hello"]]
            sql_values      [vec!["query"]]
            input           [json!( {"query": "select * from table where id = 2", "hello": "world", "hi": "there"} )]
            expected        [json!( {"query": "select * from table where id = ?", "hello": "world", "hi": "?"} )];
        ]
        [
            test_name       [test_obfuscate_json_sql_queries_2]
            keep_values     [vec![]]
            sql_values      [vec!["object"]]
            input           [json!( {"object": {"not a": "query"}} )]
            expected        [json!( {"object": {"not a": "?"}} )];
        ]
        [
            test_name       [test_obfuscate_json_sql_queries_3]
            keep_values     [vec![]]
            sql_values      [vec!["object"]]
            input           [json!( {"object": ["not", "a", "query"]} )]
            expected        [json!( {"object": ["?", "?", "?"]} )];
        ]
        [
            test_name       [test_obfuscate_json_sql_queries_4]
            keep_values     [vec!["select_id", "using_filesort", "table_name", "access_type", "possible_keys", "key", "key_length", "used_key_parts", "used_columns", "ref", "update"]]
            sql_values      [vec!["attached_condition"]]
            input           [json!( {
                "query_block": {
                  "select_id": 1,
                  "cost_info": {
                    "query_cost": "120.31"
                  },
                  "ordering_operation": {
                    "using_filesort": true,
                    "cost_info": {
                      "sort_cost": "100.00"
                    },
                    "table": {
                      "table_name": "sbtest1",
                      "access_type": "range",
                      "possible_keys": [
                        "PRIMARY"
                      ],
                      "key": "PRIMARY",
                      "used_key_parts": [
                        "id"
                      ],
                      "key_length": "4",
                      "rows_examined_per_scan": 100,
                      "rows_produced_per_join": 100,
                      "filtered": "100.00",
                      "cost_info": {
                        "read_cost": "10.31",
                        "eval_cost": "10.00",
                        "prefix_cost": "20.31",
                        "data_read_per_join": "71K"
                      },
                      "used_columns": [
                        "id",
                        "c"
                      ],
                      "attached_condition": "(`sbtest`.`sbtest1`.`id` between 5016 and 5115)"
                    }
                  }
                }
              } )]
            expected        [json!( {
                "query_block": {
                  "select_id": 1,
                  "cost_info": {
                    "query_cost": "?"
                  },
                  "ordering_operation": {
                    "using_filesort": true,
                    "cost_info": {
                      "sort_cost": "?"
                    },
                    "table": {
                      "table_name": "sbtest1",
                      "access_type": "range",
                      "possible_keys": [
                        "PRIMARY"
                      ],
                      "key": "PRIMARY",
                      "used_key_parts": [
                        "id"
                      ],
                      "key_length": "4",
                      "rows_examined_per_scan": "?",
                      "rows_produced_per_join": "?",
                      "filtered": "?",
                      "cost_info": {
                        "read_cost": "?",
                        "eval_cost": "?",
                        "prefix_cost": "?",
                        "data_read_per_join": "?"
                      },
                      "used_columns": [
                        "id",
                        "c"
                      ],
                      "attached_condition": "( sbtest . sbtest1 . id between ? and ? )"
                    }
                  }
                }
              } )];
        ]
        [
            test_name       [test_obfuscate_json_sql_queries_5]
            keep_values     [vec!["Loops", "Actual Rows", "Actual Startup Time", "Actual Total Time", "Alias", "Async Capable", "Average Sort Space Used", "Cache Evictions", "Cache Hits", "Cache Misses", "Cache Overflows", "Calls", "Command", "Conflict Arbiter Indexes", "Conflict Resolution", "Conflicting Tuples", "Constraint Name", "CTE Name", "Custom Plan Provider", "Deforming", "Emission", "Exact Heap Blocks", "Execution Time", "Expressions", "Foreign Delete", "Foreign Insert", "Foreign Update", "Full-sort Groups", "Function Call", "Function Name", "Generation", "Group Count", "Grouping Sets", "Group Key", "HashAgg Batches", "Hash Batches", "Hash Buckets", "Heap Fetches", "I/O Read Time", "I/O Write Time", "Index Name", "Inlining", "Join Type", "Local Dirtied Blocks", "Local Hit Blocks", "Local Read Blocks", "Local Written Blocks", "Lossy Heap Blocks", "Node Type", "Optimization", "Original Hash Batches", "Original Hash Buckets", "Parallel Aware", "Parent Relationship", "Partial Mode", "Peak Memory Usage", "Peak Sort Space Used", "Planned Partitions", "Planning Time", "Pre-sorted Groups", "Presorted Key", "Query Identifier", "Plan Rows", "Plan Width", "Relation Name", "Rows Removed by Conflict Filter", "Rows Removed by Filter", "Rows Removed by Index Recheck", "Rows Removed by Join Filter", "Sampling Method", "Scan Direction", "Schema", "Settings", "Shared Dirtied Blocks", "Shared Hit Blocks", "Shared Read Blocks", "Shared Written Blocks", "Single Copy", "Sort Key", "Sort Method", "Sort Methods Used", "Sort Space", "Sort Space Type", "Sort Space Used", "Startup Cost", "Strategy", "Subplan Name", "Subplans Removed", "Target Tables", "Temp Read Blocks", "Temp Written Blocks", "Time", "Timing", "Total", "Trigger", "Trigger Name", "Triggers", "Tuples Inserted", "Tuplestore Name", "Total Cost", "WAL Bytes", "WAL FPI", "WAL Records", "Worker", "Worker Number", "Workers", "Workers Launched", "Workers Planned"]]
            sql_values      [vec!["Cache Key", "Conflict Filter", "Filter", "Hash Cond", "Index Cond", "Join Filter", "Merge Cond", "Output", "Recheck Cond", "Repeatable Seed", "Sampling Parameters", "TID Cond"]]
            input           [json!( {
                "Plan": {
                  "Node Type": "Aggregate",
                  "Partial Mode": "Partial",
                  "Startup Cost": 74286.07,
                  "Total Cost": 223.59,
                  "Plan Rows": 100,
                  "Plan Width": 121,
                  "Plans": [
                    {
                      "Cache Key": "datadog.org_id",
                      "Conflict Filter": "(datadog.org_id != 8182)",
                      "Filter": "(query <> 'dogfood'::text)",
                      "Hash Cond": "(pg_stat_statements.dbid = pg_database.)",
                      "Index Cond": "((datadog.org.id >= 10) AND (datadog.org.id < 15))",
                      "Join Filter": "datadog.org.name != 'dummy'",
                      "Merge Cond": "datadog.org_name = 'dummy'",
                      "Output": ["'fakename'::text", "25", "NULL::timestamp without time zone", "NULL::text"],
                      "Recheck Cond": "datadog.org.id >= 10",
                      "Sampling Parameters": ["'15528'::real"],
                      "TID Cond": "((datadog.tid > '15531'::tid) AND (datadog.tid <= '(44247,178)'::tid))",
                      "Alias": "dog",
                      "Async Capable": true,
                      "Cache Evictions": 1,
                      "Cache Hits": 2,
                      "Cache Misses": 3,
                      "Cache Overflows": 4,
                      "Command": "Intersect",
                      "Conflict Arbiter Indexes": "dummy_index",
                      "Conflict Resolution": "NOTHING",
                      "Conflicting Tuples": 1,
                      "Constraint Name": "datadog_org.id_pkey",
                      "CTE Name": "CTE_datadog",
                      "Custom Plan Provider": "Custom Dogfood",
                      "Deforming": false,
                      "Exact Heap Blocks": 1,
                      "Execution Time": 1,
                      "Expressions": false,
                      "Foreign Delete": "datadog.org_id",
                      "Foreign Insert": "datadog.org_id",
                      "Foreign Update": "datadog.has_apm",
                      "Function Call": "count_active_users_for_product('dbm')",
                      "Function Name": "count_active_users_for_product",
                      "Group Key": ["datadog.org_id", "datadog.has_apm"],
                      "Grouping Sets": ["datadog.has_apm", "datadog.enabled_logs"],
                      "Hash Batches": 32,
                      "Hash Buckets": 8319,
                      "HashAgg Batches": 4,
                      "Heap Fetches": 8,
                      "Index Name": "dogfood",
                      "I/O Read Time": 5.31,
                      "I/O Write Time": 8.18,
                      "Join Type": "Left",
                      "Lossy Heap Blocks": 1,
                      "Original Hash Batches": 32,
                      "Original Hash Buckets": 65536,
                      "Parallel Aware": false,
                      "Parent Relationship": "Outer",
                      "Peak Memory Usage": 3941,
                      "Planned Partitions": 1,
                      "Planning Time": 0.431,
                      "Presorted Key": ["dog", "food"],
                      "Query Identifier": "3365166609774651210",
                      "Relation Name": "dog",
                      "Repeatable Seed": "'60'::double precision",
                      "Rows Removed by Conflict Filter": 1,
                      "Rows Removed by Filter": 2,
                      "Rows Removed by Index Recheck": 3,
                      "Rows Removed by Join Filter": 4,
                      "Sampling Method": "System",
                      "Scan Direction": "Forward",
                      "Schema": "dogfood_users",
                      "Settings": {
                          "enable_mergejoin": "off",
                          "enable_nestloop": "off",
                          "jit_above_cost": "0",
                          "jit_inline_above_cost": "0",
                          "jit_optimize_above_cost": "0"
                      },
                      "Single Copy": false,
                      "Sort Key": "datadog.org_id",
                      "Sort Method": "quicksort",
                      "Sort Space Type": "Memory",
                      "Sort Space Used": 2,
                      "Strategy": "Hashed",
                      "Subplan Name": "DogPlan 1",
                      "Subplans Removed": 0,
                      "Target Tables": "dog_food_users",
                      "Timing": {
                          "Generation": 1.22,
                          "Inlining": 0.1,
                          "Optimization": 0.83,
                          "Emission": 5.418,
                          "Total": 7.568
                      },
                      "Triggers": [
                          {
                             "Trigger Name": "validate_user",
                             "Relation": "datadog",
                             "Time": 1.676,
                             "Calls": 1
                          },
                          {
                             "Trigger Name": "has_apm",
                             "Relation": "datadog",
                             "Time": 1.32,
                             "Calls": 1
                          }
                      ],
                      "Trigger": {
                         "Trigger Name": "has_apm",
                         "Relation": "datadog",
                         "Time": 1.32,
                         "Calls": 1
                      },
                      "Tuples Inserted": 1,
                      "Tuplestore Name": "dog_tuples",
                      "WAL Bytes": 1,
                      "WAL FPI": 2,
                      "WAL Records": 3,
                      "Worker": {
                         "Worker Number": 0,
                         "Actual Startup Time": 303.67,
                         "Actual Total Time": 303.92,
                         "Actual Rows": 256,
                         "Actual Loops": 1
                      },
                      "Workers": [
                          {
                             "Worker Number": 0,
                             "Actual Startup Time": 1303.877,
                             "Actual Total Time": 1303.928,
                             "Actual Rows": 256,
                             "Actual Loops": 1,
                             "Full-sort Groups": {
                                "Group Count": 1,
                                "Sort Methods Used": [
                                   "quicksort"
                                ],
                                "Sort Space Memory": {
                                   "Average Sort Space Used": 34,
                                   "Peak Sort Space Used": 34
                                }
                             },
                             "Pre-sorted Groups": {
                                "Group Count": 1,
                                "Sort Methods Used": [
                                   "external merge"
                                ],
                                "Sort Space Disk": {
                                   "Average Sort Space Used": 82256,
                                   "Peak Sort Space Used": 82256
                                }
                             }
                          },
                          {
                             "Worker Number": 1,
                             "Actual Startup Time": 0.016,
                             "Actual Total Time": 51.325,
                             "Actual Rows": 294375,
                             "Actual Loops": 1,
                             "Shared Hit Blocks": 3925,
                             "Shared Read Blocks": 0,
                             "Shared Dirtied Blocks": 0,
                             "Shared Written Blocks": 0,
                             "Local Hit Blocks": 0,
                             "Local Read Blocks": 0,
                             "Local Dirtied Blocks": 0,
                             "Local Written Blocks": 0,
                             "Temp Read Blocks": 0,
                             "Temp Written Blocks": 0
                          }
                      ],
                      "Workers Launched": 5,
                      "Workers Planned": 5
                    }
                  ]
                }
              } )]
            expected        [json!( {
                "Plan": {
                  "Node Type": "Aggregate",
                  "Partial Mode": "Partial",
                  "Startup Cost": 74286.07,
                  "Total Cost": 223.59,
                  "Plan Rows": 100,
                  "Plan Width": 121,
                  "Plans": [
                    {
                      "Cache Key": "datadog.org_id",
                      "Conflict Filter": "( datadog.org_id != ? )",
                      "Filter": "( query <> ? :: text )",
                      "Hash Cond": "( pg_stat_statements.dbid = pg_database. )",
                      "Index Cond": "( ( datadog.org.id >= ? ) AND ( datadog.org.id < ? ) )",
                      "Join Filter": "datadog.org.name != ?",
                      "Merge Cond": "datadog.org_name = ?",
                      "Output": ["?", "?", "?", "?"],
                      "Recheck Cond": "datadog.org.id >= ?",
                      "Sampling Parameters": ["?"],
                      "TID Cond": "( ( datadog.tid > ? :: tid ) AND ( datadog.tid <= ? :: tid ) )",
                      "Alias": "dog",
                      "Async Capable": true,
                      "Cache Evictions": 1,
                      "Cache Hits": 2,
                      "Cache Misses": 3,
                      "Cache Overflows": 4,
                      "Command": "Intersect",
                      "Conflict Arbiter Indexes": "dummy_index",
                      "Conflict Resolution": "NOTHING",
                      "Conflicting Tuples": 1,
                      "Constraint Name": "datadog_org.id_pkey",
                      "CTE Name": "CTE_datadog",
                      "Custom Plan Provider": "Custom Dogfood",
                      "Deforming": false,
                      "Exact Heap Blocks": 1,
                      "Execution Time": 1,
                      "Expressions": false,
                      "Foreign Delete": "datadog.org_id",
                      "Foreign Insert": "datadog.org_id",
                      "Foreign Update": "datadog.has_apm",
                      "Function Call": "count_active_users_for_product('dbm')",
                      "Function Name": "count_active_users_for_product",
                      "Group Key": ["datadog.org_id", "datadog.has_apm"],
                      "Grouping Sets": ["datadog.has_apm", "datadog.enabled_logs"],
                      "Hash Batches": 32,
                      "Hash Buckets": 8319,
                      "HashAgg Batches": 4,
                      "Heap Fetches": 8,
                      "Index Name": "dogfood",
                      "I/O Read Time": 5.31,
                      "I/O Write Time": 8.18,
                      "Join Type": "Left",
                      "Lossy Heap Blocks": 1,
                      "Original Hash Batches": 32,
                      "Original Hash Buckets": 65536,
                      "Parallel Aware": false,
                      "Parent Relationship": "Outer",
                      "Peak Memory Usage": 3941,
                      "Planned Partitions": 1,
                      "Planning Time": 0.431,
                      "Presorted Key": ["dog", "food"],
                      "Query Identifier": "3365166609774651210",
                      "Relation Name": "dog",
                      "Repeatable Seed": "? :: double precision",
                      "Rows Removed by Conflict Filter": 1,
                      "Rows Removed by Filter": 2,
                      "Rows Removed by Index Recheck": 3,
                      "Rows Removed by Join Filter": 4,
                      "Sampling Method": "System",
                      "Scan Direction": "Forward",
                      "Schema": "dogfood_users",
                      "Settings": {
                          "enable_mergejoin": "off",
                          "enable_nestloop": "off",
                          "jit_above_cost": "0",
                          "jit_inline_above_cost": "0",
                          "jit_optimize_above_cost": "0"
                      },
                      "Single Copy": false,
                      "Sort Key": "datadog.org_id",
                      "Sort Method": "quicksort",
                      "Sort Space Type": "Memory",
                      "Sort Space Used": 2,
                      "Strategy": "Hashed",
                      "Subplan Name": "DogPlan 1",
                      "Subplans Removed": 0,
                      "Target Tables": "dog_food_users",
                      "Timing": {
                          "Generation": 1.22,
                          "Inlining": 0.1,
                          "Optimization": 0.83,
                          "Emission": 5.418,
                          "Total": 7.568
                      },
                      "Triggers": [
                          {
                             "Trigger Name": "validate_user",
                             "Relation": "datadog",
                             "Time": 1.676,
                             "Calls": 1
                          },
                          {
                             "Trigger Name": "has_apm",
                             "Relation": "datadog",
                             "Time": 1.32,
                             "Calls": 1
                          }
                      ],
                      "Trigger": {
                         "Trigger Name": "has_apm",
                         "Relation": "datadog",
                         "Time": 1.32,
                         "Calls": 1
                      },
                      "Tuples Inserted": 1,
                      "Tuplestore Name": "dog_tuples",
                      "WAL Bytes": 1,
                      "WAL FPI": 2,
                      "WAL Records": 3,
                      "Worker": {
                         "Worker Number": 0,
                         "Actual Startup Time": 303.67,
                         "Actual Total Time": 303.92,
                         "Actual Rows": 256,
                         "Actual Loops": 1
                      },
                      "Workers": [
                          {
                             "Worker Number": 0,
                             "Actual Startup Time": 1303.877,
                             "Actual Total Time": 1303.928,
                             "Actual Rows": 256,
                             "Actual Loops": 1,
                             "Full-sort Groups": {
                                "Group Count": 1,
                                "Sort Methods Used": [
                                   "quicksort"
                                ],
                                "Sort Space Memory": {
                                   "Average Sort Space Used": 34,
                                   "Peak Sort Space Used": 34
                                }
                             },
                             "Pre-sorted Groups": {
                                "Group Count": 1,
                                "Sort Methods Used": [
                                   "external merge"
                                ],
                                "Sort Space Disk": {
                                   "Average Sort Space Used": 82256,
                                   "Peak Sort Space Used": 82256
                                }
                             }
                          },
                          {
                             "Worker Number": 1,
                             "Actual Startup Time": 0.016,
                             "Actual Total Time": 51.325,
                             "Actual Rows": 294375,
                             "Actual Loops": 1,
                             "Shared Hit Blocks": 3925,
                             "Shared Read Blocks": 0,
                             "Shared Dirtied Blocks": 0,
                             "Shared Written Blocks": 0,
                             "Local Hit Blocks": 0,
                             "Local Read Blocks": 0,
                             "Local Dirtied Blocks": 0,
                             "Local Written Blocks": 0,
                             "Temp Read Blocks": 0,
                             "Temp Written Blocks": 0
                          }
                      ],
                      "Workers Launched": 5,
                      "Workers Planned": 5
                    }
                  ]
                }
              } )];
        ]
    )]
    #[test]
    fn test_name() {
        let mut config = ObfuscationConfig::new_test_config();
        config.elasticsearch_keep_values = Some(parse_test_args(keep_values));
        config.elasticsearch_obfuscate_sql_values = Some(parse_test_args(sql_values));
        let result = obfuscate_json_string(
            &config,
            JSONObfuscationType::Elasticsearch,
            input.to_string().as_str(),
        );
        assert_eq!(result, expected.to_string());
    }
}
