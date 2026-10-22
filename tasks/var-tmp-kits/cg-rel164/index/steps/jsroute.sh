step_1() { sed -i 's/return \[\];/return [1];/' src/handlers.js; }
step_2() { sed -i '/app.use(auth);/d' src/routes.js; }
step_3() { sed -i 's/^function listUsers/function listUsersX/' src/handlers.js; }
step_4() { sed -i 's/^function listUsersX/function listUsers/' src/handlers.js; }
step_5() { printf "function listUsers(req, res) { res.json([]); }\nmodule.exports = { listUsers };\n" > src/handlers2.js; sed -i "s#require('./handlers')#require('./handlers2')#; s/const { listUsers, auth }/const { listUsers }/" src/routes.js; }
step_6() { rm src/handlers2.js; }
step_7() { sed -i "s#require('./routes')#require('./handlers')#" src/index.js; }
step_8() { git mv -f src/routes.js src/router.js 2>/dev/null || mv src/routes.js src/router.js; sed -i "s#require('./handlers')#require('./router')#" src/index.js; }
