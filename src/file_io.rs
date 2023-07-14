use super::umi_errors::RuntimeErrors;
use anyhow::{anyhow, Context, Result};
use dialoguer::{theme::ColorfulTheme, Confirm};
use file_format::FileFormat;
use regex::Regex;
use std::{fs, path::Path, path::PathBuf};

// Defining types for simplicity
type File = std::fs::File;
type Fastq = std::io::BufReader<File>;
type Gzip = flate2::bufread::MultiGzDecoder<Fastq>;

// Enum for the two acceptable input file formats: '.fastq' and '.fastq.gz'
pub enum ReadFile {
    Fastq(std::io::BufReader<File>),
    Gzip(Box<Gzip>),
}

// Implement read for ReadFile enum
impl std::io::Read for ReadFile {
    fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
        match self {
            ReadFile::Fastq(buf_reader) => buf_reader.read(into),
            ReadFile::Gzip(buf_reader) => buf_reader.read(into),
        }
    }
}

// Enum for the two accepted output formats, '.fastq' and '.fastq.gz'
pub enum OutputFile {
    Fastq {
        read: bio::io::fastq::Writer<File>,
    },
    Gzip {
        read: bio::io::fastq::Writer<flate2::write::GzEncoder<File>>,
    },
}

// Implement write for OutputFile enum
impl OutputFile {
    pub fn write(
        self,
        header: &str,
        desc: Option<&str>,
        s: bio::io::fastq::Record,
    ) -> Result<OutputFile> {
        match self {
            OutputFile::Fastq { mut read } => match read.write(header, desc, s.seq(), s.qual()) {
                Ok(_) => Ok(OutputFile::Fastq { read }),
                Err(_) => Err(anyhow!(RuntimeErrors::ReadWriteError(s))),
            },
            OutputFile::Gzip { mut read } => match read.write(header, desc, s.seq(), s.qual()) {
                Ok(_) => Ok(OutputFile::Gzip { read }),
                Err(_) => Err(anyhow!(RuntimeErrors::ReadWriteError(s))),
            },
        }
    }
}

// Read input file to Reader. Automatically scans if input is compressed with file-format crate.
pub fn read_fastq(path: &PathBuf) -> Result<bio::io::fastq::Reader<std::io::BufReader<ReadFile>>> {
    fs::metadata(path)
        .map_err(|_e| anyhow!(RuntimeErrors::FileNotFoundError(Some(path.into()))))?;

    let format = FileFormat::from_file(path).context("Failed to determine file format")?;
    let reader: ReadFile = match format {
        FileFormat::Gzip => {
            let file = File::open(path)
                .map(std::io::BufReader::new)
                .with_context(|| format!("Failed to open file: {:?}", path))?;
            ReadFile::Gzip(Box::new(flate2::bufread::MultiGzDecoder::new(file)))
        }
        _ => {
            let file =
                File::open(path).with_context(|| format!("Failed to open file: {:?}", path))?;
            ReadFile::Fastq(std::io::BufReader::new(file))
        }
    };

    Ok(bio::io::fastq::Reader::new(reader))
}

// Create output files
pub fn output_file(name: PathBuf) -> Result<OutputFile> {
    if let Some(extension) = name.extension() {
        if extension == "gz" {
            // File has gz extension, which has been enforced by check_outputpath() if -z was provided.
            Ok(OutputFile::Gzip {
                read: std::fs::File::create(name.as_path())
                    .map(|w| flate2::write::GzEncoder::new(w, flate2::Compression::default()))
                    .map(bio::io::fastq::Writer::new)
                    .map_err(|_e| anyhow!(RuntimeErrors::OutputNotWriteableError(Some(name))))?,
            })
        } else {
            // File has extension but not gz
            Ok(OutputFile::Fastq {
                read: std::fs::File::create(name.as_path())
                    .map(bio::io::fastq::Writer::new)
                    .map_err(|_e| anyhow!(RuntimeErrors::OutputNotWriteableError(Some(name))))?,
            })
        }
    } else {
        //file has no extension. Assume plain-text.
        Ok(OutputFile::Fastq {
            read: std::fs::File::create(name.as_path())
                .map(bio::io::fastq::Writer::new)
                .map_err(|_e| anyhow!(RuntimeErrors::OutputNotWriteableError(Some(name))))?,
        })
    }
}

// Writes record with properly inserted UMI to Output file
pub fn write_to_file(
    input: bio::io::fastq::Record,
    output: OutputFile,
    umi: &[u8],
    umi_sep: Option<&String>,
    edit_nr: Option<u8>,
) -> Result<OutputFile> {
    let s = input;
    let delim = umi_sep.as_ref().map(|s| s.as_str()).unwrap_or(":"); // the delimiter for the UMI
    if let Some(number) = edit_nr {
        let header = &[s.id(), delim, std::str::from_utf8(umi).unwrap()].concat();
        let mut string = String::from(s.desc().unwrap());
        string.replace_range(0..1, &number.to_string());
        let desc: Option<&str> = Some(&string);
        output.write(header, desc, s)
    } else {
        let header = &[s.id(), delim, std::str::from_utf8(umi).unwrap()].concat();
        output.write(header, s.desc(), s.clone())
    }
}

// Checks whether an output path exists.
pub fn check_outputpath(path: PathBuf, force: &bool) -> Result<PathBuf> {
    // check if the path already exists
    let exists = fs::metadata(&path).is_ok();

    // return the path of it is ok to write, otherwise an error.
    if exists & !force {
        // force will disable prompt, but not the check.
        if Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt(format!("{} exists. Overwrite?", path.display()))
            .interact()?
        {
            println!("File will be overwritten.");
            Ok(path)
        } else {
            Err(anyhow!(RuntimeErrors::FileExistsError(Some(path))))
        }
    } else {
        Ok(path)
    }
}

// Checks whether an output path exists.
pub fn rectify_extension(mut path: PathBuf, compress: &bool) -> Result<PathBuf> {
    // handle the compression and adapt file extension if necessary.
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        match (*compress, extension.ends_with("gz")) {
            (true, false) => {
                let mut new_extension = extension.to_owned();
                new_extension.push_str(".gz");
                path.set_extension(new_extension);
            }
            (false, true) => {
                path.set_extension("");
            }
            _ => {}
        }
    } else if *compress {
        path.set_extension("gz");
    }
    Ok(path)
}

pub fn append_umi_to_path(path: &Path) -> PathBuf {
    let path_str = path.as_os_str().to_string_lossy();

    let new_path_str = if path_str.contains('\\') || path_str.contains('/') {
        // Path group: Match everything until a forward or backward slash not followed by a forward or backward slash non-greedy (*?)
        // Stem group: Match literal dot zero or one time, and everything thereafter that is not a dot, yet followed by a literal dot.
        // Extension group: Now match whatever is still left until the end $.
        let re =
            Regex::new(r"(?P<path>^.*(?:\\|/))[^/\\]*?(?P<stem>\.?[^\.]+)\.(?P<extension>.*)$")
                .unwrap();
        let new_path_str = re.replace(&path_str, "${path}${stem}_with_UMIs.${extension}");
        new_path_str
    } else {
        // Simplified regex for the cases when the file name is given without any preceding path.
        let re = Regex::new(r"(?P<stem>^\.?[^\.]+)\.(?P<extension>.*)$").unwrap();
        let new_path_str = re.replace(&path_str, "${stem}_with_UMIs.${extension}");
        new_path_str
    };
    PathBuf::from(new_path_str.to_string())
}

#[cfg(test)]
mod tests {

    use super::*;
    use assert_fs::fixture::{NamedTempFile, TempDir};
    use std::path::PathBuf;

    fn create_mock_file() -> (TempDir, NamedTempFile) {
        let temp_dir = assert_fs::TempDir::new().expect("Failed to create temporary directory");
        let mock_file = NamedTempFile::new("ACTG.fq").unwrap();
        (temp_dir, mock_file)
    }

    #[test]
    fn test_correctly_derive_output_name() {
        // plain file with simple extension
        let p = PathBuf::from("test.fastq");
        let result = append_umi_to_path(&p);
        assert_eq!(result, PathBuf::from("test_with_UMIs.fastq"));

        // plain file with multiple extensions
        let p = PathBuf::from("test.fastq.gz");
        let result = append_umi_to_path(&p);
        assert_eq!(result, PathBuf::from("test_with_UMIs.fastq.gz"));

        // path and file with multiple extensions
        let p = PathBuf::from("/some/path/test.fastq.gz");
        let result = append_umi_to_path(&p);
        assert_eq!(result, PathBuf::from("/some/path/test_with_UMIs.fastq.gz"));

        // path with hidden dir and file with multiple extensions
        let p = PathBuf::from("/some/.hidden/path/test.fastq.gz");
        let result = append_umi_to_path(&p);
        assert_eq!(
            result,
            PathBuf::from("/some/.hidden/path/test_with_UMIs.fastq.gz")
        );

        // path with hidden dir and hidden file with multiple extensions
        let p = PathBuf::from("/some/.hidden/path/.test.fastq.gz");
        let result = append_umi_to_path(&p);
        assert_eq!(
            result,
            PathBuf::from("/some/.hidden/path/.test_with_UMIs.fastq.gz")
        );

        // relative path with hidden dir and hidden file with multiple extensions
        let p = PathBuf::from("./some/.hidden/path/.test.fastq.gz");
        let result = append_umi_to_path(&p);
        assert_eq!(
            result,
            PathBuf::from("./some/.hidden/path/.test_with_UMIs.fastq.gz")
        );
    }

    #[test]
    fn test_rectify_extension() {
        let p = PathBuf::from("test.fastq");
        let result = rectify_extension(p, &false);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), PathBuf::from("test.fastq"));

        let p = PathBuf::from("test.fastq");
        let result = rectify_extension(p, &true);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), PathBuf::from("test.fastq.gz"));

        let p = PathBuf::from("test");
        let result = rectify_extension(p, &true);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), PathBuf::from("test.gz"));

        let p = PathBuf::from("test.fastq.gz");
        let result = rectify_extension(p, &false);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), PathBuf::from("test.fastq"));

        let p = PathBuf::from("test.fastq.gz");
        let result = rectify_extension(p, &true);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), PathBuf::from("test.fastq.gz"));
    }

    #[test]
    fn test_check_outputpath_existing_file_with_force() {
        let (temp_dir, file_path) = create_mock_file();
        let force = true;

        let result = check_outputpath(file_path.path().to_path_buf(), &force);

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), file_path.path().to_path_buf());

        temp_dir
            .close()
            .expect("Failed to remove temporary directory");
    }

    #[test]
    fn test_check_outputpath_new_file() {
        let (temp_dir, _file_path) = create_mock_file();
        let file_path = temp_dir.path().join("new_file");
        let force = true;

        let result = check_outputpath(file_path, &force);

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), temp_dir.path().join("new_file"));

        temp_dir
            .close()
            .expect("Failed to remove temporary directory");
    }
}
