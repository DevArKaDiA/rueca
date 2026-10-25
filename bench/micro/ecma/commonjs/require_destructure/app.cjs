const { hash, verify: check } = require("./lib.cjs");

function register(password) {
  return { digest: hash(password) };
}

function login(password, user) {
  return check(password, user.digest);
}

module.exports = { register, login };
