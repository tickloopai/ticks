//! Every tickloop package integration test, compiled as one binary.

#[path = "all/entry.rs"]
mod entry;
#[path = "all/native_visible_stdio.rs"]
mod native_visible_stdio;
#[path = "all/package.rs"]
mod package;
#[path = "../../../tests/package_skill.rs"]
mod package_skill;
