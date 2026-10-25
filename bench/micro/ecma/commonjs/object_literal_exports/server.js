const service = require("./service");

function restart() {
  service.stop();
  return service.start();
}

module.exports = { restart };
