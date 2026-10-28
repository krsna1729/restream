use restream_moq::token::*;

const KEY: TokenKey = [7; 32];
const NOW: u64 = 1_800_000_000;

#[test]
fn a_minted_token_admits_its_pipeline_until_it_expires() {
    let (token, expiry) = mint(&KEY, "pipe-1", NOW);
    assert_eq!(expiry, NOW + TOKEN_TTL.as_secs());
    assert_eq!(verify(&KEY, &token, NOW).as_deref(), Ok("pipe-1"));
    assert_eq!(verify(&KEY, &token, expiry).as_deref(), Ok("pipe-1"));
    assert_eq!(verify(&KEY, &token, expiry + 1), Err(TokenError::Expired));
}

#[test]
fn a_token_for_another_pipeline_or_key_is_forged() {
    let (token, _) = mint(&KEY, "pipe-1", NOW);
    let (other, _) = mint(&KEY, "pipe-2", NOW);
    // Pipeline 2's id with pipeline 1's MAC.
    let parts: Vec<&str> = token.split('.').collect();
    let other_parts: Vec<&str> = other.split('.').collect();
    let swapped = format!("v1.{}.{}.{}", other_parts[1], parts[2], parts[3]);
    assert_eq!(verify(&KEY, &swapped, NOW), Err(TokenError::Forged));
    assert_eq!(verify(&[8; 32], &token, NOW), Err(TokenError::Forged));
    // A later expiry under the same MAC.
    let extended = format!("v1.{}.{}.{}", parts[1], NOW + 3600, parts[3]);
    assert_eq!(verify(&KEY, &extended, NOW), Err(TokenError::Forged));
    // A correctly signed expiry beyond the TTL is never minted.
    let (far, _) = mint(&KEY, "pipe-1", NOW + 600);
    assert_eq!(verify(&KEY, &far, NOW), Err(TokenError::Forged));
}

#[test]
fn malformed_tokens_are_refused_without_panicking() {
    let (token, _) = mint(&KEY, "pipe-1", NOW);
    let truncated = &token[..token.len() - 2];
    for bad in [
        "",
        "v1",
        "v2.cGlwZQ.1.AAAA",
        "v1.cGlwZQ.notanumber.AAAA",
        "v1.!!!.1.AAAA",
        "v1.cGlwZQ.1.AAAA",
        truncated,
        &format!("{token}.extra"),
    ] {
        assert!(
            matches!(
                verify(&KEY, bad, NOW),
                Err(TokenError::Malformed | TokenError::Forged)
            ),
            "{bad:?}"
        );
    }
}

#[test]
fn the_process_key_is_stable() {
    assert_eq!(process_key(), process_key());
}
