// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use datadog_profiling::profile::*;
use lz4_flex::frame::FrameDecoder;
use prost::Message;
use std::fs::File;
use std::io::{copy, Cursor};
use std::time::Duration;

// PHP uses a restricted set of things; this helper function cuts down on a lot of typing.
fn php_location<'a>(name: &'a str, filename: &'a str, line: i64) -> api::Location<'a> {
    api::Location {
        mapping: api::Mapping::default(),
        address: 0,
        lines: vec![api::Line {
            function: api::Function {
                name,
                system_name: "",
                filename,
                start_line: 0,
            },
            line,
        }],
        is_folded: false,
    }
}

#[test]
fn wordpress() {
    let compressed_size = 101824_u64;
    let uncompressed_size = 200692_u64;
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/wordpress.pprof.lz4");
    let mut decoder = FrameDecoder::new(File::open(path).unwrap());
    let mut bytes = Vec::with_capacity(compressed_size as usize);
    let bytes_copied = copy(&mut decoder, &mut bytes).unwrap();
    assert_eq!(uncompressed_size, bytes_copied);

    let pprof = pprof::Profile::decode(&mut Cursor::new(&bytes)).unwrap();
    let api = api::Profile::try_from(&pprof).unwrap();

    assert_eq!(Duration::from_nanos(67000138417_u64), api.duration);

    let expected_sample_types = vec![
        api::ValueType {
            r#type: "sample",
            unit: "count",
        },
        api::ValueType {
            r#type: "wall-time",
            unit: "nanoseconds",
        },
        api::ValueType {
            r#type: "cpu-time",
            unit: "nanoseconds",
        },
    ];
    assert_eq!(expected_sample_types, api.sample_types);

    let expected_period = Some((
        10000000_i64,
        api::ValueType {
            r#type: "wall-time",
            unit: "nanoseconds",
        },
    ));
    assert_eq!(expected_period, api.period);

    let sample0 = api::Sample {
        locations: vec![
            /* 1 */
            php_location(
                "WP_Hook::add_filter",
                "/var/www/html/public/wp-includes/class-wp-hook.php",
                88,
            ),
            /* 2 */
            php_location(
                "add_filter",
                "/var/www/html/public/wp-includes/plugin.php",
                113,
            ),
            /* 3 */
            php_location(
                "add_action",
                "/var/www/html/public/wp-includes/plugin.php",
                404,
            ),
            /* 4 */
            php_location(
                "<?php",
                "/var/www/html/public/wp-includes/default-filters.php",
                435,
            ),
            /* 5 */ php_location("<?php", "/var/www/html/public/wp-settings.php", 136),
            /* 6 */ php_location("<?php", "/var/www/html/public/wp-config.php", 93),
            /* 7 */ php_location("<?php", "/var/www/html/public/wp-load.php", 37),
            /* 8 */ php_location("<?php", "/var/www/html/public/wp-blog-header.php", 13),
            /* 9 */ php_location("<?php", "/var/www/html/public/index.php", 17),
        ],
        values: vec![1, 6925169, 5340670],
        labels: vec![],
    };

    let actual_sample0 = api.samples.first().unwrap();
    compare_sample(sample0, actual_sample0);

    let sample1 = api::Sample {
        locations: vec![
            /* 10 */
            php_location(
                "apply_filters",
                "/var/www/html/public/wp-includes/plugin.php",
                211,
            ),
            /* 11 */
            php_location(
                "get_option",
                "/var/www/html/public/wp-includes/option.php",
                152,
            ),
            /* 12 */
            php_location(
                "WP_Widget::get_settings",
                "/var/www/html/public/wp-includes/class-wp-widget.php",
                577,
            ),
            /* 13 */
            php_location(
                "WP_Widget::_register",
                "/var/www/html/public/wp-includes/class-wp-widget.php",
                240,
            ),
            /* 14 */
            php_location(
                "WP_Widget_Factory::_register_widgets",
                "/var/www/html/public/wp-includes/class-wp-widget-factory.php",
                102,
            ),
            /* 15 */
            php_location(
                "WP_Hook::apply_filters",
                "/var/www/html/public/wp-includes/class-wp-hook.php",
                287,
            ),
            /* 16 */
            php_location(
                "WP_Hook::do_action",
                "/var/www/html/public/wp-includes/class-wp-hook.php",
                311,
            ),
            /* 17 */
            php_location(
                "do_action",
                "/var/www/html/public/wp-includes/plugin.php",
                478,
            ),
            /* 18 */
            php_location(
                "wp_widgets_init",
                "/var/www/html/public/wp-includes/widgets.php",
                1765,
            ),
            /* 15 */
            php_location(
                "WP_Hook::apply_filters",
                "/var/www/html/public/wp-includes/class-wp-hook.php",
                287,
            ),
            /* 16 */
            php_location(
                "WP_Hook::do_action",
                "/var/www/html/public/wp-includes/class-wp-hook.php",
                311,
            ),
            /* 17 */
            php_location(
                "do_action",
                "/var/www/html/public/wp-includes/plugin.php",
                478,
            ),
            /* 19 */ php_location("<?php", "/var/www/html/public/wp-settings.php", 540),
            /* 6 */ php_location("<?php", "/var/www/html/public/wp-config.php", 93),
            /* 7 */ php_location("<?php", "/var/www/html/public/wp-load.php", 37),
            /* 8 */ php_location("<?php", "/var/www/html/public/wp-blog-header.php", 13),
            /* 9 */ php_location("<?php", "/var/www/html/public/index.php", 17),
        ],
        values: vec![1, 10097441, 8518865],
        labels: vec![],
    };
    let actual_sample1 = api.samples.get(1).unwrap();
    compare_sample(sample1, actual_sample1);

    // Phew, 2 samples is hopefully enough :fingerscrossed:
}

fn compare_sample(a: api::Sample, b: &api::Sample) {
    // Comparing the entire sample works, but is bad UX when there's a failure.
    // Do it one thing at a time instead so it's a smaller diff.
    assert_eq!(a.labels, b.labels);
    assert_eq!(a.values, b.values);
    assert_eq!(a.locations.len(), b.locations.len());
    for (offset, (a, b)) in a.locations.iter().zip(b.locations.iter()).enumerate() {
        assert_eq!(
            a, b,
            "Sample location offsets {offset} differ:\n{a:#?}\n{b:#?}"
        );
    }
}
