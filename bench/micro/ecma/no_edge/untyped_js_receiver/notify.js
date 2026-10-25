class Mailer {
  send(to) {
    return `sent to ${to}`;
  }
}

function notify(o, to) {
  return o.send(to);
}

function main() {
  return notify(new Mailer(), "x@y.z");
}

module.exports = { Mailer, notify, main };
