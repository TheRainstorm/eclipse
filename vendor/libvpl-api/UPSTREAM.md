# Intel VPL public headers

Public C headers from [intel/libvpl](https://github.com/intel/libvpl/tree/674d015bcb294bc39fa276e99a652ea045423e82/api/vpl), commit `674d015bcb294bc39fa276e99a652ea045423e82` (API 2.17). Copyright Intel Corporation, MIT; see LICENSE.

API declarations are unchanged; trailing whitespace and final blank lines were normalized for repository checks.

Only API declarations are compiled into the small native adapter. The Intel dispatcher and hardware runtime are loaded dynamically from the installed system/driver; their implementations are not vendored. Header updates require an ABI review and both platform builds.
