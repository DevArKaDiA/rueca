const io = require("./io");

function copy(from, to) {
  const data = io.read(from);
  return io.write(to, data);
}

module.exports = { copy };
