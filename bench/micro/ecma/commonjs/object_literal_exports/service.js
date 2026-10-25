let running = false;

function helper(flag) {
  running = flag;
  return running;
}

module.exports = {
  start() {
    return helper(true);
  },
  stop: () => {
    return helper(false);
  },
};
