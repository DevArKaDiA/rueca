const Queue = require("./queue");

function enqueueAll(items) {
  const q = new Queue();
  for (const it of items) q.push(it);
  return q;
}

module.exports = { enqueueAll };
