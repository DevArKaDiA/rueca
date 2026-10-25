const log = require("./logger");

function start(port) {
  log("info", `listening on ${port}`);
  return port;
}

module.exports = { start };
