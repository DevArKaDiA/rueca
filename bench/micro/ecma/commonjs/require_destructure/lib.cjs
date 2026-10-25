function hash(text) {
  let h = 0;
  for (const c of text) h = (h * 31 + c.charCodeAt(0)) | 0;
  return h;
}

function verify(text, expected) {
  return hash(text) === expected;
}

module.exports = { hash, verify };
