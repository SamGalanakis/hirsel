# Finite recursive instances

Swap has two layouts, Swap[u64,bool] and Swap[bool,u64]. A leaf with bool true
and u64 7 is pushed into a Swap[u64,bool] whose a is 4 and popped back: 4*10+7=47.
Rotate has three layouts; the nested values return 4*100+5*10+7=457.
These layout names are also asserted in the front test.
Reset[T] reaches Bar[List[T]], which resets to Reset[u64], so growth never feeds
back. Its one child gives length 1. The alias First projects to its first argument;
Project[bool,String] reaches Project[bool,u64] and stabilizes, with an empty list.
Closed[bool] resets directly to Closed[u64] and returns 1 for its true label.
All values and their lists are dropped; used is zero on each engine. The app
calls the library across a package boundary.
