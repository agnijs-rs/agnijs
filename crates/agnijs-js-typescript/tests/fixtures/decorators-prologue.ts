#!/usr/bin/env agnijs
"use strict";
"another directive";
// @options: decorators script
// The helpers go after the hashbang and the directives, so "use strict"
// still applies, and the map gets an empty line there.
function mark(target: Function) {}
@mark
class Marked {}
