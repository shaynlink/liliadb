// The worker a database runs in when the page opens it (`@liliadb/wasm/worker`): it loads the
// module and serves one database over its global scope. A dedicated worker of the app's own
// can serve one the same way, with `serve(self)`.

import { serve } from "./serve.js";
import type { Endpoint } from "./messages.js";

serve(globalThis as unknown as Endpoint);
