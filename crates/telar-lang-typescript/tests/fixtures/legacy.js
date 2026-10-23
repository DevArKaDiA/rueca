'use strict';

const path = require('path');
const { readFile, writeFile: write } = require('fs/promises');
const debug = require('debug')('app');
const config = require('./config').settings;
require('./polyfills');

function Router(options) {
  this.options = options || {};
  this.stack = [];
}

/** Adds a route. */
Router.prototype.route = function route(p) {
  const layer = new Layer(p, this.options);
  this.stack.push(layer);
  return layer;
};

Router.create = function (opts) {
  return new Router(opts);
};

exports.helper = function helper(x) {
  return path.join(x, config.root);
};

async function lazy() {
  const mod = await import('./lazy.js');
  return mod.default;
}

module.exports = Router;
