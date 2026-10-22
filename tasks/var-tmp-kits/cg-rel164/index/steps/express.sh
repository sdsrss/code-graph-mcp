step_1() { sed -i '111s/^res.send = function send/res.sendx = function send/' lib/response.js; }
step_2() { sed -i '111s/^res.sendx = function send/res.send = function send/' lib/response.js; }
step_3() { cp lib/view.js ../view.bak; rm lib/view.js; }
step_4() { cp ../view.bak lib/view.js; touch lib/view.js; }
step_5() { mv lib/utils.js lib/util2.js; sed -i "s#require('./utils')#require('./util2')#" lib/*.js; sed -i "s#require('../lib/utils')#require('../lib/util2')#" test/*.js; }
step_6() { sed -i "s#require('./lib/express')#require('./lib/application')#" index.js; }
step_7() { printf "'use strict';\nexports.helper = function helper(x) { return x; };\nexports.other = (y) => exports.helper(y);\n" > lib/extra.js; sed -i "30a var helper = require('./extra').helper;" lib/response.js; sed -i "s/^res.status = function status(code) {/res.status = function status(code) {\n  helper(code);/" lib/response.js; }
step_8() { printf "exports.mock = function () {\n  var res = {};\n  res.status = function () { return res; };\n  return res;\n};\n" > test/support/mockres.js; }
step_9() { rm test/support/mockres.js; }
step_10() { sed -i "s#require('./lib/application')#require('./lib/express')#" index.js; }
