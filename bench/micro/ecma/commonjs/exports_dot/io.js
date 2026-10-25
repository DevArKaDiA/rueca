module.exports.read = function (path) {
  return exports.decode(`raw:${path}`);
};

exports.decode = (raw) => {
  return raw.slice(4);
};

exports.write = function (path, data) {
  return `${path}=${data}`;
};
