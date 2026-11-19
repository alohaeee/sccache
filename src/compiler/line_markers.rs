// Copyright 2026 sccache contributors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::borrow::Cow;

use crate::util::strip_basedirs_from_arg;

enum State<'a> {
    Normal,
    Quoted(u8),
    LineComment,
    BlockComment,
    Raw(&'a [u8]),
}

// A basedir in program data is significant. Only the filename operand of a
// preprocessor line marker may be normalized, including when comments or raw
// string literals contain text that looks like a marker.
pub(super) fn normalize_line_marker_paths<'a>(
    bytes: &'a [u8],
    basedirs: &[Vec<u8>],
) -> Cow<'a, [u8]> {
    if basedirs.is_empty()
        || bytes.starts_with(b"CPCH")
        || bytes.starts_with(b"CCOB")
        || (!bytes.starts_with(b"#") && memchr::memmem::find(bytes, b"\n#").is_none())
    {
        return Cow::Borrowed(bytes);
    }
    let mut state = State::Normal;
    let mut result = None::<Vec<u8>>;
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        match state {
            State::Normal => {
                if (i == 0 || bytes[i - 1] == b'\n')
                    && let Some((start, end, path)) = marker_path(bytes, i)
                {
                    let absolute = matches!(path.first(), Some(b'/' | b'\\'))
                        || (path.len() >= 3
                            && path[0].is_ascii_alphabetic()
                            && path[1] == b':'
                            && matches!(path[2], b'/' | b'\\'));
                    let normalized = if absolute {
                        strip_basedirs_from_arg(&path, basedirs)
                    } else {
                        Cow::Borrowed(path.as_slice())
                    };
                    if normalized.as_ref() != path.as_slice() {
                        let out = result.get_or_insert_with(|| Vec::with_capacity(bytes.len()));
                        out.extend_from_slice(&bytes[copied..start]);
                        for &c in normalized.iter() {
                            if c == b'\\' || c == b'"' {
                                out.push(b'\\');
                            }
                            out.push(c);
                        }
                        copied = end;
                    }
                    i = end + 1;
                    continue;
                }
                match bytes[i] {
                    b'R' if bytes.get(i + 1) == Some(&b'"') => {
                        let start = i + 2;
                        if let Some(end) = bytes[start..]
                            .iter()
                            .take(17)
                            .position(|&c| c == b'(')
                            .map(|n| start + n)
                            .filter(|&end| {
                                bytes[start..end]
                                    .iter()
                                    .all(|&c| c.is_ascii_graphic() && c != b')' && c != b'\\')
                            })
                        {
                            state = State::Raw(&bytes[start..end]);
                            i = end + 1;
                            continue;
                        }
                    }
                    quote @ (b'"' | b'\'') => state = State::Quoted(quote),
                    b'/' if bytes.get(i + 1) == Some(&b'/') => {
                        state = State::LineComment;
                        i += 2;
                        continue;
                    }
                    b'/' if bytes.get(i + 1) == Some(&b'*') => {
                        state = State::BlockComment;
                        i += 2;
                        continue;
                    }
                    _ => {}
                }
            }
            State::Quoted(quote) => match bytes[i] {
                b'\\' => {
                    i += escaped_len(bytes, i);
                    continue;
                }
                c if c == quote || c == b'\n' => state = State::Normal,
                _ => {}
            },
            State::LineComment => match bytes[i] {
                b'\\' => {
                    i += escaped_len(bytes, i);
                    continue;
                }
                b'\n' => state = State::Normal,
                _ => {}
            },
            State::BlockComment => {
                if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    state = State::Normal;
                    i += 2;
                    continue;
                }
            }
            State::Raw(delimiter) => {
                if bytes[i] == b')'
                    && bytes[i + 1..].starts_with(delimiter)
                    && bytes.get(i + 1 + delimiter.len()) == Some(&b'"')
                {
                    state = State::Normal;
                    i += delimiter.len() + 2;
                    continue;
                }
            }
        }
        i += 1;
    }
    match result {
        Some(mut result) => {
            result.extend_from_slice(&bytes[copied..]);
            Cow::Owned(result)
        }
        None => Cow::Borrowed(bytes),
    }
}

fn escaped_len(bytes: &[u8], i: usize) -> usize {
    if bytes.get(i + 1..i + 3) == Some(b"\r\n") {
        3
    } else {
        2.min(bytes.len() - i)
    }
}

fn marker_path(bytes: &[u8], start: usize) -> Option<(usize, usize, Vec<u8>)> {
    if bytes[start] != b'#' {
        return None;
    }
    let mut i = start + 1;
    while bytes.get(i).is_some_and(|c| matches!(c, b' ' | b'\t')) {
        i += 1;
    }
    if bytes[i..].starts_with(b"line") {
        i += 4;
        if !bytes.get(i).is_some_and(|c| matches!(c, b' ' | b'\t')) {
            return None;
        }
        while bytes.get(i).is_some_and(|c| matches!(c, b' ' | b'\t')) {
            i += 1;
        }
    }
    let number = i;
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    if i == number || !bytes.get(i).is_some_and(|c| matches!(c, b' ' | b'\t')) {
        return None;
    }
    while bytes.get(i).is_some_and(|c| matches!(c, b' ' | b'\t')) {
        i += 1;
    }
    if bytes.get(i) != Some(&b'"') {
        return None;
    }
    i += 1;
    let path_start = i;
    let mut path = Vec::new();
    loop {
        match *bytes.get(i)? {
            b'"' => return Some((path_start, i, path)),
            b'\n' | b'\r' => return None,
            b'\\' => {
                i += 1;
                let escaped = *bytes.get(i)?;
                if escaped != b'\\' && escaped != b'"' {
                    return None;
                }
                path.push(escaped);
            }
            c => path.push(c),
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_are_portable_but_program_paths_are_not() {
        let dirs = vec![b"/a/".to_vec(), b"/b/".to_vec()];
        let a = b"# 1 \"/a/main.cpp\"\nconst char* root = \"/a/\";\n";
        let b = b"# 1 \"/b/main.cpp\"\nconst char* root = \"/b/\";\n";
        assert_eq!(
            normalize_line_marker_paths(a, &dirs),
            b"# 1 \"main.cpp\"\nconst char* root = \"/a/\";\n".as_slice()
        );
        assert_ne!(
            normalize_line_marker_paths(a, &dirs),
            normalize_line_marker_paths(b, &dirs)
        );
    }

    #[test]
    fn ignores_markers_inside_literals_comments_and_pragmas() {
        let data = br###"const char* x = u8R"tag(
# 1 "/a/raw.cpp"
)tag";
/*
#line 1 "/a/comment.cpp"
*/
// \
# 1 "/a/continued-comment.cpp"
#pragma message("/a/data")
# 2 "/a/real.cpp" 1 3
"###;
        let actual = normalize_line_marker_paths(data, &[b"/a/".to_vec()]);
        let expected =
            String::from_utf8_lossy(data).replace("# 2 \"/a/real.cpp\"", "# 2 \"real.cpp\"");
        assert_eq!(actual.as_ref(), expected.as_bytes());
    }

    #[test]
    fn preserves_binary_and_unsupported_escapes() {
        let dirs = [b"/a/".to_vec()];
        for data in [
            b"CPCH\n# 1 \"/a/main.cpp\"".as_slice(),
            br#"# 1 "/a/file\t.cpp""#,
            b"# 123 ",
        ] {
            assert!(matches!(
                normalize_line_marker_paths(data, &dirs),
                Cow::Borrowed(_)
            ));
        }
    }

    #[test]
    fn supports_line_directives_and_escaped_paths() {
        let data = br#"#line 12 "/a/a\\b\"c.cpp"
"#;
        assert_eq!(
            normalize_line_marker_paths(data, &[b"/a/".to_vec()]).as_ref(),
            br#"#line 12 "a\\b\"c.cpp"
"#
        );
    }
}
