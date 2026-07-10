// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// Protocol Buffers field numbers for the `MetricData` message in the V3 format.
//
// These field numbers come from the Protocol Buffers definitions in `proto/intake_v3.proto`,
// vendored from https://github.com/DataDog/agent-payload/blob/master/proto/metrics/intake_v3.proto.
/// Field number for the `DictNameStr` column.
pub const DICT_NAME_STR_FIELD_NUMBER: u32 = 1;
/// Field number for the `DictTagsStr` column.
pub const DICT_TAGS_STR_FIELD_NUMBER: u32 = 2;
/// Field number for the `DictTagsets` column.
pub const DICT_TAGSETS_FIELD_NUMBER: u32 = 3;
/// Field number for the `DictResourceStr` column.
pub const DICT_RESOURCE_STR_FIELD_NUMBER: u32 = 4;
/// Field number for the `DictResourcesLen` column.
pub const DICT_RESOURCE_LEN_FIELD_NUMBER: u32 = 5;
/// Field number for the `DictResourceType` column.
pub const DICT_RESOURCE_TYPE_FIELD_NUMBER: u32 = 6;
/// Field number for the `DictResourceName` column.
pub const DICT_RESOURCE_NAME_FIELD_NUMBER: u32 = 7;
/// Field number for the `DictSourceTypeName` column.
pub const DICT_SOURCE_TYPE_NAME_FIELD_NUMBER: u32 = 8;
/// Field number for the `DictOriginInfo` column.
pub const DICT_ORIGIN_INFO_FIELD_NUMBER: u32 = 9;
/// Field number for the `Type` column.
pub const TYPES_FIELD_NUMBER: u32 = 10;
/// Field number for the `Name` column.
pub const NAMES_FIELD_NUMBER: u32 = 11;
/// Field number for the `Tags` column.
pub const TAGS_FIELD_NUMBER: u32 = 12;
/// Field number for the `Resources` column.
pub const RESOURCES_FIELD_NUMBER: u32 = 13;
/// Field number for the `Interval` column.
pub const INTERVALS_FIELD_NUMBER: u32 = 14;
/// Field number for the `NumPoints` column.
pub const NUM_POINTS_FIELD_NUMBER: u32 = 15;
/// Field number for the `Timestamp` column.
pub const TIMESTAMPS_FIELD_NUMBER: u32 = 16;
/// Field number for the `ValueSint64` column.
pub const VALS_SINT64_FIELD_NUMBER: u32 = 17;
/// Field number for the `ValueFloat32` column.
pub const VALS_FLOAT32_FIELD_NUMBER: u32 = 18;
/// Field number for the `ValueFloat64` column.
pub const VALS_FLOAT64_FIELD_NUMBER: u32 = 19;
/// Field number for the `SketchNBins` column.
pub const SKETCH_NUM_BINS_FIELD_NUMBER: u32 = 20;
/// Field number for the `SketchBinKeys` column.
pub const SKETCH_BIN_KEYS_FIELD_NUMBER: u32 = 21;
/// Field number for the `SketchBinCounts` column.
pub const SKETCH_BIN_CNTS_FIELD_NUMBER: u32 = 22;
/// Field number for the `SourceTypeName` column.
pub const SOURCE_TYPE_NAME_FIELD_NUMBER: u32 = 23;
/// Field number for the `OriginInfo` column.
pub const ORIGIN_INFO_FIELD_NUMBER: u32 = 24;
/// Field number for the `DictUnitStr` column.
pub const DICT_UNIT_STR_FIELD_NUMBER: u32 = 25;
/// Field number for the `UnitRef` column.
pub const UNIT_REFS_FIELD_NUMBER: u32 = 26;

/// Display names for the V3 columns, indexed by their Protocol Buffers field number.
///
/// Field numbers come from `proto/intake_v3.proto`, also mirrored in `crate::writer` as the
/// `*_FIELD_NUMBER` constants. Index 0 is unused since field numbers start at 1.
pub const COLUMN_NAMES: [&str; 27] = [
    "reserved",
    "DictNameStr",
    "DictTagsStr",
    "DictTagsets",
    "DictResourceStr",
    "DictResourcesLen",
    "DictResourceType",
    "DictResourceName",
    "DictSourceTypeName",
    "DictOriginInfo",
    "Type",
    "Name",
    "Tags",
    "Resources",
    "Interval",
    "NumPoints",
    "Timestamp",
    "ValueSint64",
    "ValueFloat32",
    "ValueFloat64",
    "SketchNBins",
    "SketchBinKeys",
    "SketchBinCounts",
    "SourceTypeName",
    "OriginInfo",
    "DictUnitStr",
    "UnitRef",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_numbers_match_column_names() {
        let pairs: &[(u32, &str)] = &[
            (DICT_NAME_STR_FIELD_NUMBER, "DictNameStr"),
            (DICT_TAGS_STR_FIELD_NUMBER, "DictTagsStr"),
            (DICT_TAGSETS_FIELD_NUMBER, "DictTagsets"),
            (DICT_RESOURCE_STR_FIELD_NUMBER, "DictResourceStr"),
            (DICT_RESOURCE_LEN_FIELD_NUMBER, "DictResourcesLen"),
            (DICT_RESOURCE_TYPE_FIELD_NUMBER, "DictResourceType"),
            (DICT_RESOURCE_NAME_FIELD_NUMBER, "DictResourceName"),
            (DICT_SOURCE_TYPE_NAME_FIELD_NUMBER, "DictSourceTypeName"),
            (DICT_ORIGIN_INFO_FIELD_NUMBER, "DictOriginInfo"),
            (TYPES_FIELD_NUMBER, "Type"),
            (NAMES_FIELD_NUMBER, "Name"),
            (TAGS_FIELD_NUMBER, "Tags"),
            (RESOURCES_FIELD_NUMBER, "Resources"),
            (INTERVALS_FIELD_NUMBER, "Interval"),
            (NUM_POINTS_FIELD_NUMBER, "NumPoints"),
            (TIMESTAMPS_FIELD_NUMBER, "Timestamp"),
            (VALS_SINT64_FIELD_NUMBER, "ValueSint64"),
            (VALS_FLOAT32_FIELD_NUMBER, "ValueFloat32"),
            (VALS_FLOAT64_FIELD_NUMBER, "ValueFloat64"),
            (SKETCH_NUM_BINS_FIELD_NUMBER, "SketchNBins"),
            (SKETCH_BIN_KEYS_FIELD_NUMBER, "SketchBinKeys"),
            (SKETCH_BIN_CNTS_FIELD_NUMBER, "SketchBinCounts"),
            (SOURCE_TYPE_NAME_FIELD_NUMBER, "SourceTypeName"),
            (ORIGIN_INFO_FIELD_NUMBER, "OriginInfo"),
            (DICT_UNIT_STR_FIELD_NUMBER, "DictUnitStr"),
            (UNIT_REFS_FIELD_NUMBER, "UnitRef"),
        ];

        for (field_number, expected_name) in pairs {
            assert_eq!(
                COLUMN_NAMES[*field_number as usize], *expected_name,
                "field number {field_number} should index COLUMN_NAMES to \"{expected_name}\""
            );
        }
    }
}
