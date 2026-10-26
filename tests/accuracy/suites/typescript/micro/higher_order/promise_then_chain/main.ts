function ok() {}
function fail() {}
Promise.resolve(1).then(ok).catch(fail);
