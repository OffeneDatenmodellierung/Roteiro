// FIXTURE — deliberately unsafe. Never imported, never run. See ../README.md.
const cp = require("child_process");

// Runs through a shell, so `branch` becomes shell syntax.
function checkout(branch) {
  return cp.exec("git checkout " + branch);
}

// Executes whatever string it is given.
function applyPolicy(source) {
  return eval(source);
}

module.exports = { checkout, applyPolicy };
