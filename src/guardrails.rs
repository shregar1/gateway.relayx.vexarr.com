use crate::providers::ChatMessage;

#[derive(Debug, Clone, PartialEq)]
pub enum GuardrailAction {
    Allow,
    Redacted(Vec<PiiDetection>),
    Blocked(Vec<PiiDetection>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PiiDetection {
    pub kind: PiiKind,
    pub message_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PiiKind {
    Ssn,
    CreditCard,
    Email,
    PhoneNumber,
    ApiKey,
}

impl PiiKind {
    fn label(self) -> &'static str {
        match self {
            PiiKind::Ssn => "SSN",
            PiiKind::CreditCard => "credit card",
            PiiKind::Email => "email",
            PiiKind::PhoneNumber => "phone number",
            PiiKind::ApiKey => "API key",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardrailMode {
    Off,
    Warn,
    Redact,
    Block,
}

impl GuardrailMode {
    pub fn from_env() -> Self {
        match std::env::var("RELAYX_GUARDRAILS_MODE")
            .unwrap_or_default()
            .as_str()
        {
            "warn" => Self::Warn,
            "redact" => Self::Redact,
            "block" => Self::Block,
            _ => Self::Off,
        }
    }
}

pub fn scan_messages(
    messages: &[ChatMessage],
    mode: GuardrailMode,
) -> (GuardrailAction, Vec<ChatMessage>) {
    if mode == GuardrailMode::Off {
        return (GuardrailAction::Allow, messages.to_vec());
    }

    let mut detections = Vec::new();
    let mut sanitized = messages.to_vec();

    for (idx, message) in messages.iter().enumerate() {
        let text = message.content_text();

        if contains_ssn(text) {
            detections.push(PiiDetection {
                kind: PiiKind::Ssn,
                message_index: idx,
            });
        }
        if contains_credit_card(text) {
            detections.push(PiiDetection {
                kind: PiiKind::CreditCard,
                message_index: idx,
            });
        }
        if contains_email(text) {
            detections.push(PiiDetection {
                kind: PiiKind::Email,
                message_index: idx,
            });
        }
        if contains_phone(text) {
            detections.push(PiiDetection {
                kind: PiiKind::PhoneNumber,
                message_index: idx,
            });
        }
        if contains_api_key(text) {
            detections.push(PiiDetection {
                kind: PiiKind::ApiKey,
                message_index: idx,
            });
        }
    }

    if detections.is_empty() {
        return (GuardrailAction::Allow, sanitized);
    }

    match mode {
        GuardrailMode::Block => (GuardrailAction::Blocked(detections), sanitized),
        GuardrailMode::Redact => {
            for msg in &mut sanitized {
                msg.content = msg.content.as_ref().map(|c| match c {
                    crate::providers::MessageContent::Text(t) => {
                        crate::providers::MessageContent::Text(redact_pii(t))
                    }
                    crate::providers::MessageContent::Blocks(blocks) => {
                        crate::providers::MessageContent::Blocks(
                            blocks.iter().map(|b| match b {
                                crate::providers::ContentBlock::Text { text } => {
                                    crate::providers::ContentBlock::Text { text: redact_pii(text) }
                                }
                                other => other.clone(),
                            }).collect()
                        )
                    }
                });
            }
            (GuardrailAction::Redacted(detections.clone()), sanitized)
        }
        GuardrailMode::Warn => {
            for det in &detections {
                tracing::warn!(
                    kind = det.kind.label(),
                    message_index = det.message_index,
                    "PII detected in request"
                );
            }
            (GuardrailAction::Allow, sanitized)
        }
        GuardrailMode::Off => unreachable!(),
    }
}

fn contains_ssn(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() < 11 {
        return false;
    }
    for i in 0..=bytes.len() - 11 {
        if bytes[i].is_ascii_digit()
            && bytes[i + 1].is_ascii_digit()
            && bytes[i + 2].is_ascii_digit()
            && bytes[i + 3] == b'-'
            && bytes[i + 4].is_ascii_digit()
            && bytes[i + 5].is_ascii_digit()
            && bytes[i + 6] == b'-'
            && bytes[i + 7].is_ascii_digit()
            && bytes[i + 8].is_ascii_digit()
            && bytes[i + 9].is_ascii_digit()
            && bytes[i + 10].is_ascii_digit()
        {
            let area: u32 = (bytes[i] - b'0') as u32 * 100
                + (bytes[i + 1] - b'0') as u32 * 10
                + (bytes[i + 2] - b'0') as u32;
            if area > 0 && area != 666 && area < 900 {
                return true;
            }
        }
    }
    false
}

fn contains_credit_card(text: &str) -> bool {
    let digits: Vec<u8> = text.bytes().filter(|b| b.is_ascii_digit()).collect();
    if digits.len() < 13 {
        return false;
    }
    for window_len in [13, 14, 15, 16] {
        if digits.len() < window_len {
            continue;
        }
        for window in digits.windows(window_len) {
            if luhn_check(window) {
                let first = window[0] - b'0';
                if matches!(first, 3 | 4 | 5 | 6) {
                    return true;
                }
            }
        }
    }
    false
}

fn luhn_check(digits: &[u8]) -> bool {
    let mut sum: u32 = 0;
    let mut double = false;
    for &d in digits.iter().rev() {
        let mut n = (d - b'0') as u32;
        if double {
            n *= 2;
            if n > 9 {
                n -= 9;
            }
        }
        sum += n;
        double = !double;
    }
    sum % 10 == 0
}

fn contains_email(text: &str) -> bool {
    for word in text.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '@' && c != '.' && c != '-' && c != '_' && c != '+');
        if let Some(at_pos) = trimmed.find('@') {
            let local = &trimmed[..at_pos];
            let domain = &trimmed[at_pos + 1..];
            if !local.is_empty() && domain.contains('.') && domain.len() > 3 {
                return true;
            }
        }
    }
    false
}

fn contains_phone(text: &str) -> bool {
    let digits: Vec<usize> = text
        .char_indices()
        .filter(|(_, c)| c.is_ascii_digit())
        .map(|(i, _)| i)
        .collect();
    if digits.len() < 10 {
        return false;
    }
    for window in digits.windows(10) {
        let span = window[9] - window[0];
        if span <= 15 {
            return true;
        }
    }
    false
}

fn contains_api_key(text: &str) -> bool {
    for word in text.split_whitespace() {
        let w = word.trim_matches(|c: char| c == '"' || c == '\'' || c == ',');
        if w.starts_with("sk-") && w.len() > 20 {
            return true;
        }
        if w.starts_with("sk-ant-") && w.len() > 20 {
            return true;
        }
        if w.starts_with("gsk_") && w.len() > 20 {
            return true;
        }
        if w.starts_with("xai-") && w.len() > 20 {
            return true;
        }
        if w.starts_with("sk-or-") && w.len() > 20 {
            return true;
        }
    }
    false
}

fn redact_pii(text: &str) -> String {
    let mut result = text.to_string();
    result = redact_ssn(&result);
    result = redact_credit_card(&result);
    result = redact_emails(&result);
    result = redact_api_keys(&result);
    result
}

fn redact_ssn(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut result = text.to_string();
    if bytes.len() < 11 {
        return result;
    }
    let mut i = bytes.len() - 1;
    loop {
        if i < 10 {
            break;
        }
        let start = i - 10;
        if bytes[start].is_ascii_digit()
            && bytes[start + 1].is_ascii_digit()
            && bytes[start + 2].is_ascii_digit()
            && bytes[start + 3] == b'-'
            && bytes[start + 4].is_ascii_digit()
            && bytes[start + 5].is_ascii_digit()
            && bytes[start + 6] == b'-'
            && bytes[start + 7].is_ascii_digit()
            && bytes[start + 8].is_ascii_digit()
            && bytes[start + 9].is_ascii_digit()
            && bytes[start + 10].is_ascii_digit()
        {
            result.replace_range(start..start + 11, "[SSN REDACTED]");
        }
        if i == 0 {
            break;
        }
        i -= 1;
    }
    result
}

fn redact_credit_card(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut digit_run = String::new();

    for ch in text.chars() {
        if ch.is_ascii_digit() || ch == '-' || ch == ' ' {
            digit_run.push(ch);
        } else {
            result.push_str(&maybe_redact_card_run(&digit_run));
            digit_run.clear();
            result.push(ch);
        }
    }
    result.push_str(&maybe_redact_card_run(&digit_run));
    result
}

fn maybe_redact_card_run(run: &str) -> String {
    let digits: Vec<u8> = run.bytes().filter(|b| b.is_ascii_digit()).collect();
    if (13..=16).contains(&digits.len()) && luhn_check(&digits) {
        let first = digits[0] - b'0';
        if matches!(first, 3 | 4 | 5 | 6) {
            return "[CARD REDACTED]".to_string();
        }
    }
    run.to_string()
}

fn redact_emails(text: &str) -> String {
    text.split_whitespace()
        .map(|word| {
            if contains_email(word) {
                "[EMAIL REDACTED]"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn redact_api_keys(text: &str) -> String {
    text.split_whitespace()
        .map(|word| {
            let w = word.trim_matches(|c: char| c == '"' || c == '\'' || c == ',');
            if (w.starts_with("sk-") || w.starts_with("sk-ant-") || w.starts_with("gsk_") || w.starts_with("xai-") || w.starts_with("sk-or-"))
                && w.len() > 20
            {
                "[API_KEY REDACTED]"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(content: &str) -> Vec<ChatMessage> {
        vec![ChatMessage::text("user", content)]
    }

    #[test]
    fn detects_ssn() {
        assert!(contains_ssn("my ssn is 123-45-6789"));
        assert!(!contains_ssn("no ssn here"));
        assert!(!contains_ssn("000-45-6789"));
        assert!(!contains_ssn("666-45-6789"));
        assert!(!contains_ssn("900-45-6789"));
    }

    #[test]
    fn detects_credit_card() {
        assert!(contains_credit_card("card: 4111 1111 1111 1111"));
        assert!(contains_credit_card("4111111111111111"));
        assert!(!contains_credit_card("1234567890"));
    }

    #[test]
    fn detects_email() {
        assert!(contains_email("send to user@example.com please"));
        assert!(!contains_email("no email here"));
        assert!(!contains_email("@ is not an email"));
    }

    #[test]
    fn detects_phone() {
        assert!(contains_phone("call me at 555-123-4567"));
        assert!(contains_phone("my number is (555) 123-4567"));
        assert!(!contains_phone("short 12345"));
    }

    #[test]
    fn detects_api_keys() {
        assert!(contains_api_key("use sk-1234567890abcdefghijklmnop"));
        assert!(contains_api_key("key: sk-ant-api03-abcdefghijklmnopqrstuvwxyz"));
        assert!(contains_api_key("groq gsk_abcdefghijklmnopqrstuvwxyz"));
        assert!(!contains_api_key("sk-short"));
    }

    #[test]
    fn block_mode_returns_blocked() {
        let messages = msg("my ssn is 123-45-6789");
        let (action, _) = scan_messages(&messages, GuardrailMode::Block);
        match action {
            GuardrailAction::Blocked(dets) => {
                assert_eq!(dets[0].kind, PiiKind::Ssn);
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn redact_mode_sanitizes_content() {
        let messages = msg("my ssn is 123-45-6789 and email user@test.com");
        let (action, sanitized) = scan_messages(&messages, GuardrailMode::Redact);
        assert!(matches!(action, GuardrailAction::Redacted(_)));
        let text = sanitized[0].content_text().to_string();
        assert!(text.contains("[SSN REDACTED]"));
        assert!(text.contains("[EMAIL REDACTED]"));
        assert!(!text.contains("123-45-6789"));
    }

    #[test]
    fn clean_content_passes_all_modes() {
        let messages = msg("What is the weather today?");
        for mode in [GuardrailMode::Warn, GuardrailMode::Redact, GuardrailMode::Block] {
            let (action, _) = scan_messages(&messages, mode);
            assert_eq!(action, GuardrailAction::Allow);
        }
    }

    #[test]
    fn off_mode_skips_scanning() {
        let messages = msg("my ssn is 123-45-6789");
        let (action, _) = scan_messages(&messages, GuardrailMode::Off);
        assert_eq!(action, GuardrailAction::Allow);
    }

    #[test]
    fn redacts_credit_card_numbers() {
        let result = redact_pii("pay with 4111 1111 1111 1111 please");
        assert!(result.contains("[CARD REDACTED]"));
        assert!(!result.contains("4111"));
    }

    #[test]
    fn redacts_api_keys() {
        let result = redact_pii("key is sk-1234567890abcdefghijklmnop");
        assert!(result.contains("[API_KEY REDACTED]"));
    }
}
