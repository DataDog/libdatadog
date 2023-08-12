pub trait TruncateUtf8 {
  fn truncate_utf8(&self, limit: usize) -> Self;
}

impl TruncateUtf8 for String {
  fn truncate_utf8(&self, limit: usize) -> Self {
    let mut bytes = 0;
    self.chars()
      .take_while(|c| {
        bytes += c.len_utf8();
        bytes <= limit
      })
      .collect::<String>()
  }
}

#[test]
fn test() {
  assert_eq!("", String::from("").truncate_utf8(5));
	assert_eq!("télé", String::from("télé").truncate_utf8(6));
	assert_eq!("t", String::from("télé").truncate_utf8(2));
	assert_eq!("éé", String::from("ééééé").truncate_utf8(5));
	assert_eq!("ééééé", String::from("ééééé").truncate_utf8(18));
	assert_eq!("ééééé", String::from("ééééé").truncate_utf8(10));
	assert_eq!("ééé", String::from("ééééé").truncate_utf8(6));
}
