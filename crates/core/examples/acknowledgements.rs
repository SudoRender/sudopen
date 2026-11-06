//! Prints the licenses of the syntax definitions and themes bundled through
//! two-face, as Markdown. Used by scripts/notices.sh.
fn main() {
    print!("{}", two_face::acknowledgement::listing().to_md());
}
