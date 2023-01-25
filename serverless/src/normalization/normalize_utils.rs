use crate::errors;

// DEFAULT_SPAN_NAME is the default name we assign a span if it's missing and we have no reasonable fallback
pub const DEFAULT_SPAN_NAME: &str = "unnamed_operation";
// DEFAULT_SERVICE_NAME is the default name we assign a service if it's missing and we have no reasonable fallback
pub const DEFAULT_SERVICE_NAME: &str = "unnamed-service";

// MAX_NAME_LEN the maximum length a name can have
pub const MAX_NAME_LEN: i64 = 100;
// MAX_SERVICE_LEN the maximum length a service can have
pub const MAX_SERVICE_LEN: i64 = 100;

pub const MAX_TAG_LEN: i64 = 200;

// TruncateUTF8 truncates the given string to make sure it uses less than limit bytes.
// If the last character is an utf8 character that would be splitten, it removes it
// entirely to make sure the resulting string is not broken.
pub fn truncate_utf8(s: String, limit: i64) -> String {
    if s.len() <= limit as usize {
        return s
    }
    let mut prev_index = 0;
    for i in 0..s.len() {
        if i > limit as usize {
            return s[0..prev_index].to_string();
        }
        prev_index = i;
    }
    return s;
}

// NormalizeService normalizes a span service and returns an error describing the reason
// (if any) why the name was modified.
pub fn normalize_service(svc: String, lang: String) -> (String, Option<errors::NormalizeErrors>) {
    if svc == "" {
        return (fallback_service(lang), Some(errors::NormalizeErrors::ErrorEmpty));
    }

    let mut truncated_service = svc.clone();
    let mut err: Option<errors::NormalizeErrors> = None;

    if svc.len() > MAX_SERVICE_LEN as usize {
        truncated_service = truncate_utf8(svc, MAX_SERVICE_LEN);
        err = errors::NormalizeErrors::ErrorTooLong.into();
    }

    let normalized_service = normalize_tag(truncated_service);
    if normalized_service == "" {
        return (fallback_service(lang), Some(errors::NormalizeErrors::ErrorInvalid));
    }
    return (normalized_service, err);
}

// fallbackService returns the fallback service name for a service
// belonging to language lang.
pub fn fallback_service(lang: String) -> String {
    if lang == "" {
		return DEFAULT_SERVICE_NAME.to_string();
	}
    let mut service_name = String::new();
    service_name.push_str("unnamed-");
    service_name.push_str(&lang);
    service_name.push_str("-service");
    // TODO: the original golang implementation uses a map to cache previously created
    // service names. Implement that here.
    return service_name;
}

// normalize_name normalizes a span name and returns an error describing the reason
// (if any) why the name was modified.
pub fn normalize_name(name: String) -> (String, Option<errors::NormalizeErrors>) {
    if name.is_empty() {
        return (DEFAULT_SPAN_NAME.to_string(), errors::NormalizeErrors::ErrorEmpty.into());
    }
    let mut truncated_name = name.clone();
    let mut err: Option<errors::NormalizeErrors> = None;

    if name.len() > MAX_NAME_LEN as usize {
        truncated_name = truncate_utf8(name.clone(), MAX_NAME_LEN);
        err = errors::NormalizeErrors::ErrorTooLong.into();
    }

    let (normalized_name, ok) = normalize_metric_names(truncated_name.clone());
    if !ok {
        return (DEFAULT_SPAN_NAME.to_string(), errors::NormalizeErrors::ErrorInvalid.into())
    }
    return (normalized_name, err);
}

// NormalizeTag applies some normalization to ensure the tags match the backend requirements.
// TODO: The implementation differs from the original go implementation. Verify this satisfies all
//       backend tag format requirements and no edge cases are missed.
pub fn normalize_tag(tag: String) -> String {
    // Fast path: Check if the tag is valid and only contains ASCII characters,
	// if yes return it as-is right away. For most use-cases this reduces CPU usage.
	if is_normalized_ascii_tag(tag.clone()) {
		return tag;
	}

    if tag.len() == 0 {
        return "".to_string();
    }

    // given a dummy value
    let mut last_char: char = 'a';

    let mut result = String::with_capacity(tag.len());

    let char_vec: Vec<char> = tag.chars().collect();

    for i in 0..char_vec.len() {
        if result.len() == MAX_TAG_LEN as usize {
            break;
        }
        if char_vec[i].is_lowercase() {
            result.push(char_vec[i]);
            last_char = char_vec[i];
            continue;
        }
        if char_vec[i].is_uppercase() {
            let mut iter = char_vec[i].to_lowercase();
            if iter.len() == 1 {
                let c: char = iter.next().unwrap();
                result.push(c);
                last_char = c;
            }
            continue;
        }
        if char_vec[i].is_alphabetic() {
            result.push(char_vec[i]);
            last_char = char_vec[i];
            continue;
        }
        if char_vec[i] == ':' {
            result.push(char_vec[i]);
            last_char = char_vec[i];
            continue;
        }
        if result.len() > 0 && (char_vec[i].is_ascii_digit() || char_vec[i] == '.' || char_vec[i] == '/' || char_vec[i] == '-') {
            result.push(char_vec[i]);
            last_char = char_vec[i];
            continue;
        }
        if result.len() > 0 && last_char != '_' {
            result.push('_');
            last_char = '_';
        }
    }

    if last_char == '_' {
        result.remove(result.len() - 1);
    }

    return result.to_string();
}

pub fn is_normalized_ascii_tag(tag: String) -> bool {
    if tag.len() == 0 {
        return true;
    }
    if tag.len() > MAX_TAG_LEN as usize {
        return false;
    }
    if !is_valid_ascii_start_char(tag.chars().nth(0).unwrap()) {
        return false;
    }
    for mut i in 0..tag.len() {
        let b: char = tag.chars().nth(i).unwrap();
        if is_valid_ascii_tag_char(b) {
            continue;
        }
        if b == '_' {
            // an underscore is only okay if followed by a valid non-underscore character
			i+=1;
			if i == tag.len() || !is_valid_ascii_tag_char(tag.chars().nth(i).unwrap()) {
				return false;
			}
        } else {
            return false;
        }
    }
    return true;
}

pub fn is_valid_ascii_start_char(c: char) -> bool {
    return ('a' <= c && c <= 'z') || c == ':';
}

pub fn is_valid_ascii_tag_char(c: char) -> bool {
    return is_valid_ascii_start_char(c) || ('0' <= c && c <= '9') || c == '.' || c == '/' || c == '-';
}

pub fn normalize_metric_names(name: String) -> (String, bool) {
    if name == "" || name.len() > MAX_NAME_LEN as usize {
        return (name, false);
    }

    // rust efficient ways to build strings, see here:
    // https://github.com/hoodie/concatenation_benchmarks-rs
    let mut result = String::with_capacity(name.len());

    // given a dummy value
    let mut last_char: char = 'a';

    let char_vec: Vec<char> = name.chars().collect();

    let mut i = 0;

    // skip non-alphabetic characters
    while i < name.len() && !is_alpha(char_vec[0]) {
        i+=1;
    }

    // if there were no alphabetic characters it wasn't valid
    if i == name.len() {
        return ("".to_string(), false);
    }

    while i < name.len() {
        if is_alpha_num(char_vec[i]) {
            result.push(char_vec[i]);
            last_char = char_vec[i];
        } else if char_vec[i] == '.' {
            // we skipped all non-alpha chars up front so we have seen at least one
            if last_char == '_' {
                // overwrite underscores that happen before periods
                result.replace_range((result.len()-1)..(result.len()), ".");
                last_char = '.'
            } else {
                result.push('.');
                last_char = '.';
            }
        } else {
            // we skipped all non-alpha chars up front so we have seen at least one
            if last_char != '.' && last_char != '_' {
                result.push('_');
                last_char = '_';
            }
        }
        i+=1;
    }

    if last_char == '_' {
        result.remove(result.len() - 1);
    }
    return (result, true);
}

pub fn is_alpha(c: char) -> bool {
    return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z');
}

pub fn is_alpha_num(c: char) -> bool {
    return is_alpha(c) || (c >= '0' && c <= '9');
}
