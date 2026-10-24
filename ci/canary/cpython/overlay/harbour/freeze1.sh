#!/bin/sh
# Stage 1: freeze the four modules _bootstrap_python needs, using the host
# program Harbour built for this step.
set -e
mkdir -p Python/frozen_modules
"$HARBOUR_TOOL_FREEZE_MODULE" getpath ./Modules/getpath.py Python/frozen_modules/getpath.h
"$HARBOUR_TOOL_FREEZE_MODULE" importlib._bootstrap ./Lib/importlib/_bootstrap.py Python/frozen_modules/importlib._bootstrap.h
"$HARBOUR_TOOL_FREEZE_MODULE" importlib._bootstrap_external ./Lib/importlib/_bootstrap_external.py Python/frozen_modules/importlib._bootstrap_external.h
"$HARBOUR_TOOL_FREEZE_MODULE" zipimport ./Lib/zipimport.py Python/frozen_modules/zipimport.h
