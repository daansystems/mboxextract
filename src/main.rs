use clap::Parser;
use mailparse::{dateparse, MailHeaderMap, ParsedMail};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Component, Path, PathBuf};

const UNKNOWN_DATE: &str = "unknown";

#[derive(Parser, Debug)]
#[command(
    name = "mboxextract",
    about = "Extract MIME attachments from an mbox file into date-based directories"
)]
struct Args {
    /// Path to the mbox file
    mbox: PathBuf,

    /// Output directory
    #[arg(short, long, default_value = ".")]
    output: PathBuf,

    /// Overwrite files that already exist
    #[arg(long, conflicts_with = "skip_existing")]
    overwrite: bool,

    /// Skip files whose target already exists
    #[arg(long, conflicts_with = "overwrite")]
    skip_existing: bool,

    /// Print the name of every extracted file
    #[arg(short, long)]
    verbose: bool,
}

fn is_mbox_separator(line: &[u8]) -> bool {
    if !line.starts_with(b"From ") {
        return false;
    }

    let line = String::from_utf8_lossy(line);
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 7 {
        return false;
    }

    matches!(
        fields[2],
        "Mon" | "Tue" | "Wed" | "Thu" | "Fri" | "Sat" | "Sun"
    ) && fields[6].chars().all(|c| c.is_ascii_digit())
}

fn envelope_date(line: &[u8]) -> Option<(i64, i64, i64)> {
    let line = String::from_utf8_lossy(line);
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 7 {
        return None;
    }

    let month = match fields[3] {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let day = fields[4].parse::<i64>().ok()?;
    let year = fields[6].parse::<i64>().ok()?;
    Some((year, month, day))
}

fn date_from_mail(message: &ParsedMail<'_>, envelope: Option<&[u8]>) -> String {
    if let Some(value) = message.get_headers().get_first_value("Date") {
        if let Ok(timestamp) = dateparse(&value) {
            if let Some(date) = timestamp_to_date(timestamp) {
                return date;
            }
        }
    }

    if let Some(line) = envelope {
        if let Some((year, month, day)) = envelope_date(line) {
            return format!("{year:04}-{month:02}-{day:02}");
        }
    }

    UNKNOWN_DATE.to_owned()
}

fn timestamp_to_date(timestamp: i64) -> Option<String> {
    let seconds_per_day = 86_400i64;
    let mut z = timestamp.div_euclid(seconds_per_day) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    z = yoe;
    let mut y = z + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    y += if m <= 2 { 1 } else { 0 };
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

fn safe_filename(name: &str) -> String {
    let name = name.trim().replace(['/', '\\', ':', '\0'], "_");
    let name = Path::new(&name)
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("_");
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        "attachment".to_owned()
    } else {
        name.chars().take(240).collect()
    }
}

fn unique_path(directory: &Path, filename: &str) -> PathBuf {
    let destination = directory.join(filename);
    if !destination.exists() {
        return destination;
    }

    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment");
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();

    for index in 1_u64.. {
        let candidate = format!("{stem}-{index}{extension}");
        let destination = directory.join(candidate);
        if !destination.exists() {
            return destination;
        }
    }
    unreachable!()
}

fn extract_from_mail(
    mail: &ParsedMail<'_>,
    directory: &Path,
    overwrite: bool,
    skip_existing: bool,
    verbose: bool,
) -> Result<usize, String> {
    let mut count = 0;
    for part in mail.subparts.iter() {
        if !part.subparts.is_empty() {
            count += extract_from_mail(part, directory, overwrite, skip_existing, verbose)?;
            continue;
        }

        let disposition = part.get_content_disposition();
        if disposition.disposition != mailparse::DispositionType::Attachment
            && disposition.params.get("filename").is_none()
        {
            continue;
        }

        let raw_name = disposition
            .params
            .get("filename")
            .map(String::as_str)
            .unwrap_or("attachment");
        let filename = safe_filename(raw_name);

        let mut destination = if overwrite {
            directory.join(&filename)
        } else if skip_existing {
            directory.join(&filename)
        } else {
            unique_path(directory, &filename)
        };

        if skip_existing && destination.exists() {
            if verbose {
                println!("Skipping existing file: {}", destination.display());
            }
            continue;
        }

        if destination.exists() && !overwrite && !skip_existing {
            destination = unique_path(directory, &filename);
        }

        let data = match part.get_body_raw() {
            Ok(data) => data,
            Err(error) => {
                eprintln!(
                    "Skipping attachment {filename}: failed to decode Base64 or MIME body: {error}"
                );
                continue;
            }
        };
        fs::create_dir_all(directory)
            .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
        let file = File::create(&destination)
            .map_err(|error| format!("cannot create {}: {error}", destination.display()))?;
        let mut writer = BufWriter::new(file);
        writer
            .write_all(&data)
            .map_err(|error| format!("cannot write {}: {error}", destination.display()))?;
        writer
            .flush()
            .map_err(|error| format!("cannot flush {}: {error}", destination.display()))?;

        count += 1;
        if verbose {
            println!("{}", destination.display());
        }
    }
    Ok(count)
}

fn run(args: Args) -> Result<(), String> {
    if !args.mbox.is_file() {
        return Err(format!("mbox file not found: {}", args.mbox.display()));
    }

    let input = File::open(&args.mbox)
        .map_err(|error| format!("cannot open {}: {error}", args.mbox.display()))?;
    let mut reader = BufReader::with_capacity(64 * 1024, input);
    let mut message = Vec::new();
    let mut envelope: Option<Vec<u8>> = None;
    let mut message_count = 0;
    let mut attachment_count = 0;
    let mut saw_separator = false;
    let mut line = Vec::new();

    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(|error| format!("cannot read {}: {error}", args.mbox.display()))?;
        if read == 0 {
            break;
        }

        if is_mbox_separator(&line) {
            if saw_separator && !message.is_empty() {
                process_message(
                    &message,
                    envelope.as_deref(),
                    &args.output,
                    args.overwrite,
                    args.skip_existing,
                    args.verbose,
                    &mut message_count,
                    &mut attachment_count,
                )?;
                message.clear();
            }
            envelope = Some(line.clone());
            saw_separator = true;
        } else if saw_separator {
            message.extend_from_slice(&line);
        }
    }

    if saw_separator && !message.is_empty() {
        process_message(
            &message,
            envelope.as_deref(),
            &args.output,
            args.overwrite,
            args.skip_existing,
            args.verbose,
            &mut message_count,
            &mut attachment_count,
        )?;
    }

    println!(
        "Processed {message_count} message(s); extracted {attachment_count} attachment(s) into {}",
        args.output.display()
    );
    Ok(())
}

fn process_message(
    message: &[u8],
    envelope: Option<&[u8]>,
    output: &Path,
    overwrite: bool,
    skip_existing: bool,
    verbose: bool,
    message_count: &mut usize,
    attachment_count: &mut usize,
) -> Result<(), String> {
    let mail = parse_mail(message).map_err(|error| format!("cannot parse email: {error}"))?;
    let date = date_from_mail(&mail, envelope);
    let directory = output.join(date);
    let count = extract_from_mail(&mail, &directory, overwrite, skip_existing, verbose)?;
    *message_count += 1;
    *attachment_count += count;
    Ok(())
}

fn parse_mail(message: &[u8]) -> Result<ParsedMail<'_>, mailparse::MailParseError> {
    mailparse::parse_mail(message)
}

fn main() {
    let args = Args::parse();
    if let Err(error) = run(args) {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separator_requires_mbox_shape() {
        assert!(is_mbox_separator(
            b"From sender@example.com Mon Jan  1 12:00:00 2024\n"
        ));
        assert!(!is_mbox_separator(b"From text inside the body\n"));
    }

    #[test]
    fn filename_removes_path_characters() {
        assert_eq!(safe_filename("../bad\\name.txt"), ".._bad_name.txt");
        assert_eq!(safe_filename(""), "attachment");
    }

    #[test]
    fn timestamp_conversion_is_stable() {
        assert_eq!(
            timestamp_to_date(1_705_406_400).as_deref(),
            Some("2024-01-16")
        );
    }

    #[test]
    fn extracts_base64_attachment() {
        let raw = b"Date: Tue, 16 Jan 2024 12:00:00 +0000\n\
MIME-Version: 1.0\n\
Content-Type: multipart/mixed; boundary=\"b\"\n\n\
--b\n\
Content-Type: text/plain\n\n\
body\n\
--b\n\
Content-Type: application/octet-stream\n\
Content-Disposition: attachment; filename=\"data.bin\"\n\
Content-Transfer-Encoding: base64\n\n\
AP8=\n\
--b--\n";
        let mail = parse_mail(raw).unwrap();
        let directory =
            std::env::temp_dir().join(format!("mboxextract-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);

        let count = extract_from_mail(&mail, &directory, true, false, false).unwrap();
        assert_eq!(count, 1);
        assert_eq!(fs::read(directory.join("data.bin")).unwrap(), b"\0\xff");

        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn skips_attachment_when_base64_decoding_fails() {
        let raw = b"MIME-Version: 1.0\n\
Content-Type: multipart/mixed; boundary=\"b\"\n\n\
--b\n\
Content-Type: text/plain\n\n\
body\n\
--b\n\
Content-Type: application/octet-stream\n\
Content-Disposition: attachment; filename=\"broken.bin\"\n\
Content-Transfer-Encoding: base64\n\n\
A\n\
--b--\n";
        let mail = parse_mail(raw).unwrap();
        let directory =
            std::env::temp_dir().join(format!("mboxextract-invalid-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);

        let count = extract_from_mail(&mail, &directory, true, false, false).unwrap();
        assert_eq!(count, 0);
        assert!(!directory.join("broken.bin").exists());

        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn skips_existing_attachment() {
        let raw = b"MIME-Version: 1.0\n\
Content-Type: multipart/mixed; boundary=\"b\"\n\n\
--b\n\
Content-Type: application/octet-stream\n\
Content-Disposition: attachment; filename=\"existing.bin\"\n\
Content-Transfer-Encoding: base64\n\n\
AP8=\n\
--b--\n";
        let mail = parse_mail(raw).unwrap();
        let directory =
            std::env::temp_dir().join(format!("mboxextract-existing-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("existing.bin"), b"original").unwrap();

        let count = extract_from_mail(&mail, &directory, false, true, false).unwrap();
        assert_eq!(count, 0);
        assert_eq!(
            fs::read(directory.join("existing.bin")).unwrap(),
            b"original"
        );

        let _ = fs::remove_dir_all(directory);
    }
}
