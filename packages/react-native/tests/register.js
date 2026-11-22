// Runs the tests with `react-native` resolved to `react-native.js`, which stands in for the app.
import { registerHooks } from "node:module";

const mock = new URL("./react-native.js", import.meta.url).href;

registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier === "react-native") return { url: mock, shortCircuit: true };
    return nextResolve(specifier, context);
  },
});
