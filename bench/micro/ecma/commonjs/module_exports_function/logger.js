function format(level, msg) {
  return `[${level}] ${msg}`;
}

module.exports = function log(level, msg) {
  console.log(format(level, msg));
};
