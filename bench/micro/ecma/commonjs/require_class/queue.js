class Queue {
  constructor() {
    this.items = [];
    this.reset();
  }

  reset() {
    this.items.length = 0;
  }

  push(item) {
    this.items.push(item);
    return this.items.length;
  }
}

module.exports = Queue;
