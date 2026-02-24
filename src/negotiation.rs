//! Content negotiation for image format selection.
//!
//! Parses the client `Accept` header (RFC 7231 quality values) to determine
//! which output format to use for image conversion.

use crate::config::OutputFormat;

/// A parsed media range with quality value.
#[derive(Debug, Clone)]
struct MediaRange {
    /// Media type (e.g., "image/webp", "image/*", "*/*").
    media_type: String,
    /// Quality value (0.0 - 1.0, default 1.0).
    quality: f32,
}

/// Parse an Accept header into a list of media ranges with quality values.
fn parse_accept(accept: &str) -> Vec<MediaRange> {
    accept
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }

            let mut segments = part.split(';');
            let media_type = segments.next()?.trim().to_lowercase();

            let mut quality = 1.0_f32;
            for param in segments {
                let param = param.trim();
                if let Some(q_val) = param.strip_prefix("q=") {
                    quality = q_val.trim().parse().unwrap_or(1.0);
                }
            }

            Some(MediaRange {
                media_type,
                quality,
            })
        })
        .collect()
}

/// Check if a media range matches a specific content type.
fn matches_type(range: &str, content_type: &str) -> bool {
    if range == "*/*" || range == content_type {
        return true;
    }
    // Check for wildcard subtypes like "image/*"
    if let Some(prefix) = range.strip_suffix("/*") {
        if let Some(ct_prefix) = content_type.split('/').next() {
            return prefix == ct_prefix;
        }
    }
    false
}

/// Negotiate the best output format given the client's Accept header and
/// the configured format priority list.
///
/// Returns `None` if the client doesn't support any configured format.
pub fn negotiate_format(accept_header: Option<&str>, configured_formats: &[OutputFormat]) -> Option<OutputFormat> {
    let accept = match accept_header {
        Some(h) if !h.is_empty() => h,
        // No Accept header means the client accepts anything
        _ => return configured_formats.first().copied(),
    };

    let ranges = parse_accept(accept);

    // For each configured format (in priority order), check if the client
    // accepts it with a non-zero quality value.
    for &format in configured_formats {
        let content_type = format.content_type();

        // Find the best matching range for this format
        let best_q = ranges
            .iter()
            .filter(|r| matches_type(&r.media_type, content_type))
            .map(|r| r.quality)
            .reduce(f32::max);

        if let Some(q) = best_q {
            if q > 0.0 {
                return Some(format);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_accept_header_returns_first_format() {
        let formats = vec![OutputFormat::WebP, OutputFormat::Avif];
        assert_eq!(negotiate_format(None, &formats), Some(OutputFormat::WebP));
    }

    #[test]
    fn empty_accept_header_returns_first_format() {
        let formats = vec![OutputFormat::WebP, OutputFormat::Avif];
        assert_eq!(negotiate_format(Some(""), &formats), Some(OutputFormat::WebP));
    }

    #[test]
    fn explicit_webp_support() {
        let formats = vec![OutputFormat::WebP, OutputFormat::Avif];
        let accept = "image/webp, image/png, image/jpeg";
        assert_eq!(negotiate_format(Some(accept), &formats), Some(OutputFormat::WebP));
    }

    #[test]
    fn explicit_avif_support_when_webp_not_accepted() {
        let formats = vec![OutputFormat::WebP, OutputFormat::Avif];
        let accept = "image/avif, image/png, image/jpeg";
        assert_eq!(negotiate_format(Some(accept), &formats), Some(OutputFormat::Avif));
    }

    #[test]
    fn wildcard_image_accepts_first_format() {
        let formats = vec![OutputFormat::WebP, OutputFormat::Avif];
        let accept = "image/*, text/html";
        assert_eq!(negotiate_format(Some(accept), &formats), Some(OutputFormat::WebP));
    }

    #[test]
    fn global_wildcard_accepts_first_format() {
        let formats = vec![OutputFormat::Avif, OutputFormat::WebP];
        let accept = "*/*";
        assert_eq!(negotiate_format(Some(accept), &formats), Some(OutputFormat::Avif));
    }

    #[test]
    fn quality_zero_excludes_format() {
        let formats = vec![OutputFormat::WebP, OutputFormat::Avif];
        // Client explicitly rejects webp but accepts avif
        let accept = "image/webp;q=0, image/avif;q=0.8, image/jpeg";
        assert_eq!(negotiate_format(Some(accept), &formats), Some(OutputFormat::Avif));
    }

    #[test]
    fn no_supported_format_returns_none() {
        let formats = vec![OutputFormat::WebP, OutputFormat::Avif];
        let accept = "image/jpeg, image/png";
        assert_eq!(negotiate_format(Some(accept), &formats), None);
    }

    #[test]
    fn quality_values_parsed_correctly() {
        let formats = vec![OutputFormat::WebP];
        let accept = "image/webp;q=0.9, image/jpeg;q=1.0";
        assert_eq!(negotiate_format(Some(accept), &formats), Some(OutputFormat::WebP));
    }

    #[test]
    fn complex_accept_header() {
        let formats = vec![OutputFormat::WebP, OutputFormat::Avif];
        let accept = "text/html, application/xhtml+xml, image/avif, image/webp, image/apng, */*;q=0.8";
        assert_eq!(negotiate_format(Some(accept), &formats), Some(OutputFormat::WebP));
    }

    #[test]
    fn empty_formats_returns_none() {
        assert_eq!(negotiate_format(Some("*/*"), &[]), None);
    }
}
