#!/bin/sh
# Stage 2: freeze the rest, using _bootstrap_python.
#
# CPython only treats an executable as being in a build tree -- and so finds
# `Lib/` -- when it sits at the source root beside these two landmarks, so the
# tool Harbour built is copied here.
set -e
mkdir -p Python/frozen_modules
cp "$HARBOUR_TOOL_BOOTSTRAP_PYTHON" ./_bootstrap_python
touch Modules/Setup.local
echo none > pybuilddir.txt
./_bootstrap_python ./Programs/_freeze_module.py abc ./Lib/abc.py Python/frozen_modules/abc.h
./_bootstrap_python ./Programs/_freeze_module.py codecs ./Lib/codecs.py Python/frozen_modules/codecs.h
./_bootstrap_python ./Programs/_freeze_module.py io ./Lib/io.py Python/frozen_modules/io.h
./_bootstrap_python ./Programs/_freeze_module.py _collections_abc ./Lib/_collections_abc.py Python/frozen_modules/_collections_abc.h
./_bootstrap_python ./Programs/_freeze_module.py _sitebuiltins ./Lib/_sitebuiltins.py Python/frozen_modules/_sitebuiltins.h
./_bootstrap_python ./Programs/_freeze_module.py genericpath ./Lib/genericpath.py Python/frozen_modules/genericpath.h
./_bootstrap_python ./Programs/_freeze_module.py ntpath ./Lib/ntpath.py Python/frozen_modules/ntpath.h
./_bootstrap_python ./Programs/_freeze_module.py posixpath ./Lib/posixpath.py Python/frozen_modules/posixpath.h
./_bootstrap_python ./Programs/_freeze_module.py os ./Lib/os.py Python/frozen_modules/os.h
./_bootstrap_python ./Programs/_freeze_module.py site ./Lib/site.py Python/frozen_modules/site.h
./_bootstrap_python ./Programs/_freeze_module.py stat ./Lib/stat.py Python/frozen_modules/stat.h
./_bootstrap_python ./Programs/_freeze_module.py importlib.util ./Lib/importlib/util.py Python/frozen_modules/importlib.util.h
./_bootstrap_python ./Programs/_freeze_module.py importlib.machinery ./Lib/importlib/machinery.py Python/frozen_modules/importlib.machinery.h
./_bootstrap_python ./Programs/_freeze_module.py runpy ./Lib/runpy.py Python/frozen_modules/runpy.h
./_bootstrap_python ./Programs/_freeze_module.py __hello__ ./Lib/__hello__.py Python/frozen_modules/__hello__.h
./_bootstrap_python ./Programs/_freeze_module.py __phello__ ./Lib/__phello__/__init__.py Python/frozen_modules/__phello__.h
./_bootstrap_python ./Programs/_freeze_module.py __phello__.ham ./Lib/__phello__/ham/__init__.py Python/frozen_modules/__phello__.ham.h
./_bootstrap_python ./Programs/_freeze_module.py __phello__.ham.eggs ./Lib/__phello__/ham/eggs.py Python/frozen_modules/__phello__.ham.eggs.h
./_bootstrap_python ./Programs/_freeze_module.py __phello__.spam ./Lib/__phello__/spam.py Python/frozen_modules/__phello__.spam.h
./_bootstrap_python ./Programs/_freeze_module.py frozen_only ./Tools/freeze/flag.py Python/frozen_modules/frozen_only.h
