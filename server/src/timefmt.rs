//! Fechas sin dependencias: segundos Unix → calendario civil (UTC).

const DIAS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MESES: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

pub struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub min: u32,
    pub sec: u32,
    pub wday: usize,
}

pub fn civil(secs: i64) -> Civil {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400) as u32;
    // Howard Hinnant, «days_from_civil» inverso
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    Civil {
        year,
        month,
        day,
        hour: rem / 3600,
        min: rem % 3600 / 60,
        sec: rem % 60,
        wday: (days + 4).rem_euclid(7) as usize,
    }
}

pub fn unix(t: std::time::SystemTime) -> i64 {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX / 2),
        Err(e) => -i64::try_from(e.duration().as_secs()).unwrap_or(i64::MAX / 2),
    }
}

/// `Sun, 06 Nov 1994 08:49:37 GMT`
pub fn http_date(secs: i64) -> String {
    let c = civil(secs);
    format!("{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT", DIAS[c.wday], c.day, MESES[(c.month - 1) as usize], c.year, c.hour, c.min, c.sec)
}

/// `1994-11-06 08:49`
pub fn short(secs: i64) -> String {
    let c = civil(secs);
    format!("{:04}-{:02}-{:02} {:02}:{:02}", c.year, c.month, c.day, c.hour, c.min)
}

/// Hora y fecha MS-DOS (los años fuera de 1980..2107 se recortan).
pub fn dos(secs: i64) -> (u16, u16) {
    let c = civil(secs);
    if c.year < 1980 {
        return (0, (1 << 5) | 1);
    }
    let y = (c.year.min(2107) - 1980) as u16;
    let time = ((c.hour as u16) << 11) | ((c.min as u16) << 5) | (c.sec as u16 / 2);
    let date = (y << 9) | ((c.month as u16) << 5) | c.day as u16;
    (time, date)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fechas_conocidas() {
        assert_eq!(http_date(784_111_777), "Sun, 06 Nov 1994 08:49:37 GMT");
        assert_eq!(short(0), "1970-01-01 00:00");
        assert_eq!(http_date(951_782_400), "Tue, 29 Feb 2000 00:00:00 GMT");
        assert_eq!(dos(-5), (0, 33));
    }
}
