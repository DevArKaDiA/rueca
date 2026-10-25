const strings = require("./strings");

function greet(name) {
  return strings.shout(`hello ${name}`);
}

function greetWith(moduleName, name) {
  const mod = require(`./${moduleName}`);
  return mod.shout(name);
}

module.exports = { greet, greetWith };
