pub(super) fn seconds_to_ms(seconds: f64) -> Result<Option<i64>, String> {
    let ms = seconds * 1000.0;
    if !ms.is_finite() || ms > i64::MAX as f64 || ms < i64::MIN as f64 {
        return Err("start_time is out of range".to_string());
    }
    Ok(Some(ms.round() as i64))
}

pub fn parse_start_time_ms(input: &str) -> Result<Option<i64>, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    if let Ok(seconds) = trimmed.parse::<f64>() {
        if !seconds.is_finite() {
            return Err("start_time must be a finite number".to_string());
        }
        if seconds < 0.0 {
            return Err("start_time must be non-negative".to_string());
        }
        return seconds_to_ms(seconds);
    }

    let parts: Vec<&str> = trimmed.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return Err("start_time must be seconds or MM:SS(.mmm) or HH:MM:SS(.mmm)".to_string());
    }

    let seconds = parts[parts.len() - 1]
        .parse::<f64>()
        .map_err(|_| "invalid seconds component in start_time".to_string())?;
    if !seconds.is_finite() {
        return Err("start_time must be a finite number".to_string());
    }
    if seconds < 0.0 {
        return Err("start_time must be non-negative".to_string());
    }

    let minutes = parts[parts.len() - 2]
        .parse::<i64>()
        .map_err(|_| "invalid minutes component in start_time".to_string())?;
    if minutes < 0 {
        return Err("start_time must be non-negative".to_string());
    }

    let hours = if parts.len() == 3 {
        let value = parts[0]
            .parse::<i64>()
            .map_err(|_| "invalid hours component in start_time".to_string())?;
        if value < 0 {
            return Err("start_time must be non-negative".to_string());
        }
        value
    } else {
        0
    };

    let hours_secs = hours
        .checked_mul(3600)
        .ok_or_else(|| "start_time is out of range".to_string())?;
    let minutes_secs = minutes
        .checked_mul(60)
        .ok_or_else(|| "start_time is out of range".to_string())?;
    let total_secs_int = hours_secs
        .checked_add(minutes_secs)
        .ok_or_else(|| "start_time is out of range".to_string())?;

    seconds_to_ms(total_secs_int as f64 + seconds)
}
