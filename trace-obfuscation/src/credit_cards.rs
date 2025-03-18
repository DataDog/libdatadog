// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// is_card_number checks if b could be a credit card number by checking the digit count and IIN
/// prefix. If validate_luhn is true, the Luhn checksum is also applied to potential candidates.
/// Note: This code is based on the code from datadog-agent/pkg/obfuscate/credit_cards.go
pub fn is_card_number<T: AsRef<str>>(s: T, validate_luhn: bool) -> bool {
    let s = s.as_ref();
    if s.len() < 12 {
        // fast path
        return false;
    }

    let mut num_s = [0; 17];
    let mut len = 0;
    for c in s.chars() {
        // Only valid characters are 0-9, space (" ") and dash("-")
        #[allow(clippy::unwrap_used)]
        match c {
            ' ' | '-' => continue,
            '0'..='9' => {
                num_s[len] = c.to_digit(10).unwrap();
                len += 1;
            }
            _ => return false,
        };
        if len > 16 {
            // too long for any known card number; stop looking
            return false;
        }
    }
    if len < 12 {
        // need at least 12 digits to be a valid card number
        return false;
    }

    let num_s = &num_s[..len];
    let mut is_valid_iin = FuzzyBool::Maybe;
    let mut cs = num_s.iter();

    #[allow(clippy::unwrap_used)]
    let mut prefix: u32 = *cs.next().unwrap();
    #[allow(clippy::unwrap_used)]
    while is_valid_iin == FuzzyBool::Maybe {
        is_valid_iin = valid_card_prefix(prefix);

        prefix = 10 * prefix + cs.next().unwrap();
    }

    if is_valid_iin == FuzzyBool::True && validate_luhn {
        return luhn_valid(num_s);
    }
    is_valid_iin == FuzzyBool::True
}

/// luhnValid checks that the number represented in the given vector validates the Luhn Checksum
/// algorithm. nums must be non-empty
///
/// See:
/// https://en.wikipedia.org/wiki/Luhn_algorithm
fn luhn_valid(nums: &[u32]) -> bool {
    #[allow(clippy::unwrap_used)]
    let (given_digit, payload) = nums.split_last().unwrap();
    calculate_luhn(payload) == *given_digit
}

/// Calculate the luhn checksum from a given payload
/// Note that any existing checksum digit should already be removed from the payload provided
fn calculate_luhn(payload: &[u32]) -> u32 {
    let mut acc = 0;
    for (i, val) in payload.iter().rev().enumerate() {
        let x = if i % 2 == 0 {
            let dbl_x = val * 2;
            if dbl_x > 9 {
                (dbl_x % 10) + 1
            } else {
                dbl_x
            }
        } else {
            *val
        };
        acc += x;
    }
    10 - (acc % 10) % 10
}

#[derive(Debug, PartialEq)]
enum FuzzyBool {
    True,
    Maybe,
    False,
}

/// validCardPrefix validates whether b is a valid card prefix. Returns Maybe if
/// the prefix could be an Issuer Identification Number(IIN) once more digits are revealed and
/// reports True if b is a fully valid IIN.
///
/// IMPORTANT: If adding new prefixes to this algorithm, make sure that you update
/// the "maybe" clauses above, in the shorter prefixes than the one you are adding.
/// This refers to the cases which return Maybe.
///
/// TODO(x): this whole code could be code generated from a prettier data structure.
/// Ultimately, it could even be user-configurable.
/// Note: This code is ported from datadog-agent/pkg/obfuscate/credit_cards.go
fn valid_card_prefix(n: u32) -> FuzzyBool {
    // Validates IIN prefix possibilities
    // Source: https://www.regular-expressions.info/creditcard.html
    if n > 699999 {
        // too long for any known prefix; stop looking
        return FuzzyBool::False;
    }
    if n < 10 {
        return match n {
            1 | 4 => FuzzyBool::True,
            2 | 3 | 5 | 6 => FuzzyBool::Maybe,
            _ => FuzzyBool::False,
        };
    }
    if n < 100 {
        return match n {
            34..=39 | 51..=55 | 62 | 65 => FuzzyBool::True,
            30 | 63 | 64 | 50 | 60 | 22..=27 | 56..=58 | 60..=69 => FuzzyBool::Maybe,
            _ => FuzzyBool::False,
        };
    }
    if n < 1000 {
        return match n {
            300..=305 | 309 | 636 | 644..=649 => FuzzyBool::True,
            352..=358 | 501 | 601 | 222..=272 | 500..=509 | 560..=589 | 600..=699 => {
                FuzzyBool::Maybe
            }
            _ => FuzzyBool::False,
        };
    }
    if n < 10000 {
        return match n {
            3528..=3589 | 5019 | 6011 => FuzzyBool::True,
            2221..=2720 | 5000..=5099 | 5600..=5899 | 6000..=6999 => FuzzyBool::Maybe,
            _ => FuzzyBool::False,
        };
    }
    if n < 100000 {
        return match n {
            22210..=27209 | 50000..=50999 | 56000..=58999 | 60000..=69999 => FuzzyBool::Maybe,
            _ => FuzzyBool::False,
        };
    }
    if n < 1000000 {
        return match n {
            222100..=272099 | 500000..=509999 | 560000..=589999 | 600000..=699999 => {
                FuzzyBool::True
            }
            _ => FuzzyBool::False,
        };
    }
    // unknown IIN
    FuzzyBool::False
}

#[cfg(test)]
mod tests {
    use crate::credit_cards::{calculate_luhn, is_card_number, valid_card_prefix, FuzzyBool};

    #[test]
    fn test_valid_card_prefix() {
        let test_cases = vec![
            (1, FuzzyBool::True),
            (4, FuzzyBool::True),
            // maybe
            (2, FuzzyBool::Maybe),
            (3, FuzzyBool::Maybe),
            (5, FuzzyBool::Maybe),
            (6, FuzzyBool::Maybe),
            // no
            (7, FuzzyBool::False),
            (8, FuzzyBool::False),
            (9, FuzzyBool::False),
            // yes
            (34, FuzzyBool::True),
            (37, FuzzyBool::True),
            (39, FuzzyBool::True),
            (51, FuzzyBool::True),
            (55, FuzzyBool::True),
            (62, FuzzyBool::True),
            (65, FuzzyBool::True),
            // maybe
            (30, FuzzyBool::Maybe),
            (63, FuzzyBool::Maybe),
            (22, FuzzyBool::Maybe),
            (27, FuzzyBool::Maybe),
            (69, FuzzyBool::Maybe),
            // no
            (31, FuzzyBool::False),
            (29, FuzzyBool::False),
            (21, FuzzyBool::False),
            // yes
            (300, FuzzyBool::True),
            (305, FuzzyBool::True),
            (644, FuzzyBool::True),
            (649, FuzzyBool::True),
            (309, FuzzyBool::True),
            (636, FuzzyBool::True),
            // maybe
            (352, FuzzyBool::Maybe),
            (358, FuzzyBool::Maybe),
            (501, FuzzyBool::Maybe),
            (601, FuzzyBool::Maybe),
            (222, FuzzyBool::Maybe),
            (272, FuzzyBool::Maybe),
            (500, FuzzyBool::Maybe),
            (509, FuzzyBool::Maybe),
            (560, FuzzyBool::Maybe),
            (589, FuzzyBool::Maybe),
            (600, FuzzyBool::Maybe),
            (699, FuzzyBool::Maybe),
            // yes
            (3528, FuzzyBool::True),
            (3589, FuzzyBool::True),
            (5019, FuzzyBool::True),
            (6011, FuzzyBool::True),
            // maybe
            (2221, FuzzyBool::Maybe),
            (2720, FuzzyBool::Maybe),
            (5000, FuzzyBool::Maybe),
            (5099, FuzzyBool::Maybe),
            (5600, FuzzyBool::Maybe),
            (5899, FuzzyBool::Maybe),
            (6000, FuzzyBool::Maybe),
            (6999, FuzzyBool::Maybe),
            // maybe
            (22210, FuzzyBool::Maybe),
            (27209, FuzzyBool::Maybe),
            (50000, FuzzyBool::Maybe),
            (50999, FuzzyBool::Maybe),
            (56000, FuzzyBool::Maybe),
            (58999, FuzzyBool::Maybe),
            (60000, FuzzyBool::Maybe),
            (69999, FuzzyBool::Maybe),
            // no
            (21000, FuzzyBool::False),
            (55555, FuzzyBool::False),
            // yes
            (222100, FuzzyBool::True),
            (272099, FuzzyBool::True),
            (500000, FuzzyBool::True),
            (509999, FuzzyBool::True),
            (560000, FuzzyBool::True),
            (589999, FuzzyBool::True),
            (600000, FuzzyBool::True),
            (699999, FuzzyBool::True),
            // no
            (551234, FuzzyBool::False),
            (594388, FuzzyBool::False),
            (219899, FuzzyBool::False),
        ];

        for (num, expected) in test_cases {
            let actual = valid_card_prefix(num);
            assert_eq!(
                actual, expected,
                "card prefix '{}' was expected to be {:?} but got {:?}",
                num, expected, actual
            )
        }
    }

    #[test]
    fn test_invalid_cards() {
        let invalid_cards = vec![
            "37828224631000521389798", // valid but too long
            "37828224631",             // valid but too short
            "   3782822-4631 ",
            "3714djkkkksii31",  // invalid character
            "x371413321323331", // invalid characters
            "",
            "7712378231899",
            "   -  ",
            "3714djkkkksii3三",
        ];
        for invalid_card in invalid_cards {
            assert!(
                !is_card_number(invalid_card, false),
                "invalid card '{}' was detected as valid",
                invalid_card
            );
        }
    }

    #[test]
    fn test_valid_cards() {
        let valid_cards = vec![
            "378282246310005",
            "  378282246310005",
            "  3782-8224-6310-005 ",
            "371449635398431",
            "378734493671000",
            "5610591081018250",
            "30569309025904",
            "38520000023237",
            "6011 1111 1111 1117",
            "6011000990139424",
            " 3530111333--300000  ",
            "3566002020360505",
            "5555555555554444",
            "5105-1051-0510-5100",
            " 4111111111111111",
            "4012888888881881 ",
            "422222 2222222",
            "5019717010103742",
            "6331101999990016",
            " 4242-4242-4242-4242 ",
            "4242-4242-4242-4242 ",
            "4242-4242-4242-4242  ",
            "4000056655665556",
            "5555555555554444",
            "2223003122003222",
            "5200828282828210",
            "5105105105105100",
            "378282246310005",
            "371449635398431",
            "6011111111111117",
            "6011000990139424",
            "3056930009020004",
            "3566002020360505",
            "620000000000000",
            "2222 4053 4324 8877",
            "2222 9909 0525 7051",
            "2223 0076 4872 6984",
            "2223 5771 2001 7656",
            "5105 1051 0510 5100",
            "5111 0100 3017 5156",
            "5185 5408 1000 0019",
            "5200 8282 8282 8210",
            "5204 2300 8000 0017",
            "5204 7400 0990 0014",
            "5420 9238 7872 4339",
            "5455 3307 6000 0018",
            "5506 9004 9000 0436",
            "5506 9004 9000 0444",
            "5506 9005 1000 0234",
            "5506 9208 0924 3667",
            "5506 9224 0063 4930",
            "5506 9274 2731 7625",
            "5553 0422 4198 4105",
            "5555 5537 5304 8194",
            "5555 5555 5555 4444",
            "4012 8888 8888 1881",
            "4111 1111 1111 1111",
            "6011 0009 9013 9424",
            "6011 1111 1111 1117",
            "3714 496353 98431",
            "3782 822463 10005",
            "3056 9309 0259 04",
            "3852 0000 0232 37",
            "3530 1113 3330 0000",
            "3566 0020 2036 0505",
            "3700 0000 0000 002",
            "3700 0000 0100 018",
            "6703 4444 4444 4449",
            "4871 0499 9999 9910",
            "4035 5010 0000 0008",
            "4360 0000 0100 0005",
            "6243 0300 0000 0001",
            "5019 5555 4444 5555",
            "3607 0500 0010 20",
            "6011 6011 6011 6611",
            "6445 6445 6445 6445",
            "5066 9911 1111 1118",
            "6062 8288 8866 6688",
            "3569 9900 1009 5841",
            "6771 7980 2100 0008",
            "2222 4000 7000 0005",
            "5555 3412 4444 1115",
            "5577 0000 5577 0004",
            "5555 4444 3333 1111",
            "2222 4107 4036 0010",
            "5555 5555 5555 4444",
            "2222 4107 0000 0002",
            "2222 4000 1000 0008",
            "2223 0000 4841 0010",
            "2222 4000 6000 0007",
            "2223 5204 4356 0010",
            "2222 4000 3000 0004",
            "5100 0600 0000 0002",
            "2222 4000 5000 0009",
            "1354 1001 4004 955",
            "4111 1111 4555 1142",
            "4988 4388 4388 4305",
            "4166 6766 6766 6746",
            "4646 4646 4646 4644",
            "4000 6200 0000 0007",
            "4000 0600 0000 0006",
            "4293 1891 0000 0008",
            "4988 0800 0000 0000",
            "4111 1111 1111 1111",
            "4444 3333 2222 1111",
            "4001 5900 0000 0001",
            "4000 1800 0000 0002",
            "4000 0200 0000 0000",
            "4000 1600 0000 0004",
            "4002 6900 0000 0008",
            "4400 0000 0000 0008",
            "4484 6000 0000 0004",
            "4607 0000 0000 0009",
            "4977 9494 9494 9497",
            "4000 6400 0000 0005",
            "4003 5500 0000 0003",
            "4000 7600 0000 0001",
            "4017 3400 0000 0003",
            "4005 5190 0000 0006",
            "4131 8400 0000 0003",
            "4035 5010 0000 0008",
            "4151 5000 0000 0008",
            "4571 0000 0000 0001",
            "4199 3500 0000 0002",
            "4001 0200 0000 0009",
        ];
        for valid_card in valid_cards {
            assert!(
                is_card_number(valid_card, false),
                "valid card '{}' was detected as invalid",
                valid_card
            );
        }
    }

    #[test]
    fn test_calculate_luhn() {
        let actual = calculate_luhn(&[7, 9, 9, 2, 7, 3, 9, 8, 7, 1]);
        assert_eq!(actual, 3);
    }
}
