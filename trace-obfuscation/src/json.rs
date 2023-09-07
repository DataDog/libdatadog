// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

use std::collections::HashSet;

use serde_json::{json, Value};

pub fn obfuscate_json(json_str: &str, keep_values: Vec<String>) -> String {
    let mut json_dict: Value = serde_json::from_str(json_str).unwrap_or_default();
    if json_dict.is_null() {
        return "?".to_string();
    }
    recurse_and_replace_json(&mut json_dict, &HashSet::from_iter(keep_values));

    json_dict.to_string()
}

fn recurse_and_replace_json(value: &mut Value, keep_values: &HashSet<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if !keep_values.contains(k) {
                    if !v.is_array() && !v.is_object() {
                        *v = json!("?");
                    }
                    recurse_and_replace_json(v, keep_values);
                }
            }
        }
        Value::Array(arr) => {
            for (_, v) in arr.iter_mut().enumerate() {
                if !v.is_array() && !v.is_object() {
                    *v = json!("?");
                }
                recurse_and_replace_json(v, keep_values);
            }
        }
        _ => (),
    }
}

#[cfg(test)]
mod tests {
    use duplicate::duplicate_item;
    use serde_json::json;

    use super::obfuscate_json;

    #[duplicate_item(
        [
            test_name       [test_obfuscate_json_1]
            keep_values     [vec!["other".to_string()]]
            input           [json!({
                "query": {
                    "multi_match": {
                        "query": "guide",
                        "fields": [
                            "_all",
                            {
                                "key": "value",
                                "other": ["1", "2", {"k": "v"}]
                            },
                            "2"
                            ],
                    }
                }
            })]
            expected        [json!({
                "query": {
                    "multi_match": {
                        "query": "?",
                        "fields": [
                            "?",
                            {
                                "key": "?",
                                "other": ["1", "2", {"k": "v"}]
                            },
                            "?"
                            ],
                    }
                }
            }).to_string()];
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
            }).to_string()];
        ]
        [
            test_name       [test_obfuscate_json_3]
            keep_values     [vec!["other".to_string()]]
            input           [json!({ "query": { "multi_match" : { "query" : "guide", "fields" : ["_all", { "key": "value", "other": ["1", "2", {"k": "v"}] }, "2"] } } } )]
            expected        [json!({ "query": { "multi_match": { "query": "?", "fields" : ["?", { "key": "?", "other": ["1", "2", {"k": "v"}] }, "?"] } } }).to_string()];
        ]
        [
            test_name       [test_obfuscate_json_4]
            keep_values     [vec!["fields".to_string()]]
            input           [json!({"fields" : ["_all", { "key": "value", "other": ["1", "2", {"k": "v"}] }, "2"]})]
            expected        [json!({"fields" : ["_all", { "key": "value", "other": ["1", "2", {"k": "v"}] }, "2"]}).to_string()];
        ]
        [
            test_name       [test_obfuscate_json_5]
            keep_values     [vec!["k".to_string()]]
            input           [json!({"fields" : ["_all", { "key": "value", "other": ["1", "2", {"k": "v"}] }, "2"]})]
            expected        [json!({"fields" : ["?", { "key": "?", "other": ["?", "?", {"k": "v"}] }, "?"]}).to_string()];
        ]
        [
            test_name       [test_obfuscate_json_6]
            keep_values     [vec!["C".to_string()]]
            input           [json!({"fields" : [{"A": 1, "B": {"C": 3}}, "2"]})]
            expected        [json!({"fields" : [{"A": "?", "B": {"C": 3}}, "?"]}).to_string()];
        ]
        [
            test_name       [test_obfuscate_json_7]
            keep_values     [vec![]]
            input           [json!({
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
            })]
            expected        [json!({
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
            }).to_string()];
        ]
        [
            test_name       [test_obfuscate_json_8]
            keep_values     [vec!["_source".to_string()]]
            input           [json!({
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
            })]
            expected        [json!({
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
            }).to_string()];
        ]
        [
            test_name       [test_obfuscate_json_9]
            keep_values     [vec!["query".to_string()]]
            input           [json!({
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
            })]
            expected        [json!({
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
            }).to_string()];
        ]
        [
            test_name       [test_obfuscate_json_10]
            keep_values     [vec!["match".to_string()]]
            input           [json!({
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
            })]
            expected        [json!({
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
            }).to_string()];
        ]
        [
            test_name       [test_obfuscate_json_11]
            keep_values     [vec!["hits".to_string()]]
            input           [json!({
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
            })]
            expected        [json!({
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
            }).to_string()];
        ]
        [
            test_name       [test_obfuscate_json_12]
            keep_values     [vec!["_index".to_string(), "title".to_string()]]
            input           [json!({
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
            })]
            expected        [json!({
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
            }).to_string()];
        ]
        [
            test_name       [test_obfuscate_json_13]
            keep_values     [vec!["_source".to_string()]]
            input           [json!({
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
              })]
            expected        [json!({
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
              }).to_string()];
        ]
        [
            test_name       [test_obfuscate_json_14]
            keep_values     [vec!["C".to_string()]]
            input           ["not valid json"]
            expected        ["?".to_string()];
        ]
        [
            test_name       [test_obfuscate_json_15]
            keep_values     [vec!["C".to_string()]]
            input           [json!({"fields" : [{"A": 1, "B": {"C": 3}}, "2"]})]
            expected        [json!({"fields" : [{"A": "?", "B": {"C": 3}}, "?"]}).to_string()];
        ]
    )]
    #[test]
    fn test_name() {
        let result = obfuscate_json(input.to_string().as_str(), keep_values);
        assert_eq!(result, expected);
    }
}
