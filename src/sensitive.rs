//! Recognises field and variable names that carry secrets, so every surface
//! that renders ServiceNow data (the TUI, catalog plans, variable templates)
//! redacts the same set.

/// Placeholder shown instead of a sensitive value.
pub const REDACTED: &str = "[REDACTED]";

/// Whether a field or variable name looks like it holds a secret.
pub fn is_sensitive_field(field: &str) -> bool {
    // Any punctuation separates words, so a label such as "Enter your PIN:"
    // yields the token "pin".
    let normalized: String = field
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect();
    normalized.contains("password")
        || normalized.contains("passwd")
        || normalized.contains("secret")
        || normalized.contains("private_key")
        || normalized.contains("api_key")
        || normalized.contains("apikey")
        || normalized.contains("access_key")
        || normalized.contains("passphrase")
        || normalized.contains("passcode")
        || normalized.split('_').any(|part| {
            matches!(
                part,
                "token"
                    | "cookie"
                    | "credential"
                    | "credentials"
                    | "authorization"
                    | "pwd"
                    | "pw"
                    | "pass"
                    | "pin"
                    | "ssn"
                    | "otp"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_like_names_are_sensitive() {
        for field in [
            "password",
            "u_client_secret",
            "access_token",
            "api-key",
            "session_cookie",
            "private_key_pem",
            "Set Password",
            "vpn_pass",
            "admin pw",
            "Enter your PIN:",
            "Auth token.",
            "OTP (SMS)",
            "VPN pass,",
            "api.key",
            "AWS Credential",
            "passphrase",
            "vpn_passcode",
            "user_pwd",
            "PIN",
            "badge-pin",
            "ssn",
            "aws_access_key",
            "otp",
            "service_credentials",
        ] {
            assert!(is_sensitive_field(field), "{field} should be sensitive");
        }
    }

    #[test]
    fn names_that_only_contain_a_keyword_fragment_are_not_sensitive() {
        for field in [
            "tokenized_name",
            "short_description",
            "cookies_enabled",
            "pinned",
            "shipping_address",
            "spin_count",
            "passport_number",
            "bypass_reason",
            "power_supply",
        ] {
            assert!(
                !is_sensitive_field(field),
                "{field} should not be sensitive"
            );
        }
    }
}
