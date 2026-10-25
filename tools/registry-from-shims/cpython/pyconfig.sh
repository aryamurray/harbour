#!/bin/sh
# Composes generated/pyconfig.h: a platform's asserted answers, then the header
# Harbour's probes wrote.
#
#   sh harbour/pyconfig.sh harbour/pyconfig/<platform>.h
#
# The probe-generated header's path is HARBOUR_PROBE_HEADER, set by Harbour for a
# target that declares probes with a generated header.
set -e
literals="${1:?usage: pyconfig.sh <literals header>}"
if [ ! -f "$literals" ]; then
  echo "pyconfig.sh: no asserted pyconfig answers for this platform (wanted $literals)" >&2
  exit 1
fi
if [ -z "$HARBOUR_PROBE_HEADER" ] || [ ! -f "$HARBOUR_PROBE_HEADER" ]; then
  echo "pyconfig.sh: HARBOUR_PROBE_HEADER is not set to a header: the probes did not run" >&2
  exit 1
fi
mkdir -p generated
{
  echo "/* pyconfig.h: composed by harbour/pyconfig.sh from $literals and the probe-generated header. */"
  echo "#ifndef Py_PYCONFIG_H"
  echo "#define Py_PYCONFIG_H"
  cat "$literals"
  cat "$HARBOUR_PROBE_HEADER"
  echo "#endif /* Py_PYCONFIG_H */"
} > generated/pyconfig.h
