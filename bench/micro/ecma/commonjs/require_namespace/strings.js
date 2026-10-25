exports.upper = function (s) {
  return s.toUpperCase();
};

exports.shout = function (s) {
  return exports.upper(s) + "!";
};
