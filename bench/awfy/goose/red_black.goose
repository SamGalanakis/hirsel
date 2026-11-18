// The CD red-black tree (AWFY JavaScript cd.js). Index zero represents nil.
namespace red_black;

enum Order { Less, Equal, Greater }
struct Links { children: i64[2] = [0, 0], parent: i64 = 0, red: bool = false }
struct Entry<K, V> { key: K, value: V }

root struct Tree<K, V> {
    root: i64 = 0,
    links: Links[>..] = [],
    entries: Entry<K, V>[>..] = []
}

fn initialize<K, V>(tree: Tree<K, V>&) {
    tree.links.push(Links {});
}

fn minimum(links: const Links[:], start: i64) -> i64 {
    var current = start;
    while links[current].children[0] != 0 { current = links[current].children[0]; }
    return current;
}

fn first<K, V>(tree: Tree<K, V>&) -> i64 {
    return minimum(tree.links, tree.root);
}

fn next<K, V>(tree: Tree<K, V>&, start: i64) -> i64 {
    let right = tree.links[start].children[1];
    if right != 0 { return minimum(tree.links, right); }
    var node = start;
    var parent = tree.links[node].parent;
    while parent != 0 && node == tree.links[parent].children[1] {
        node = parent;
        parent = tree.links[node].parent;
    }
    return parent;
}

fn find<C, K, V>(tree: Tree<K, V>&, key: K) -> i64 {
    var node = tree.root;
    while node != 0 {
        match C(key, tree.entries[node - 1].key) {
            Less => { node = tree.links[node].children[0]; },
            Greater => { node = tree.links[node].children[1]; },
            Equal => { return node; }
        }
    }
    return 0;
}

fn attach(links: Links[>..]&, root: i64&, parent: i64, old: i64, replacement: i64) {
    if parent == 0 {
        root = replacement;
    } else {
        let side = if links[parent].children[0] == old { 0 } else { 1 };
        links[parent].children[side] = replacement;
    }
}

fn rotate(links: Links[>..]&, root: i64&, node: i64, side: i64) {
    let other = 1 - side;
    let child = links[node].children[other];
    let middle = links[child].children[side];
    let parent = links[node].parent;
    links[node].children[other] = middle;
    if middle != 0 {
        links[middle].parent = node;
    }
    links[child].parent = parent;
    attach(links, root, parent, node, child);
    links[child].children[side] = node;
    links[node].parent = child;
}

fn balance_insert(links: Links[>..]&, root: i64&, inserted: i64) {
    var node = inserted;
    while node != root && links[links[node].parent].red {
        let parent = links[node].parent;
        let grandparent = links[parent].parent;
        let side = if links[grandparent].children[0] == parent { 0 } else { 1 };
        let uncle = links[grandparent].children[1 - side];
        if links[uncle].red {
            links[parent].red = false;
            links[uncle].red = false;
            links[grandparent].red = true;
            node = grandparent;
        } else {
            if node == links[parent].children[1 - side] {
                node = parent;
                rotate(links, root, node, side);
            }
            let p = links[node].parent;
            let g = links[p].parent;
            links[p].red = false;
            links[g].red = true;
            rotate(links, root, g, 1 - side);
        }
    }
    links[root].red = false;
}

fn balance_delete(links: Links[>..]&, root: i64&, replacement: i64, parent: i64) {
    var node = replacement;
    var p = parent;
    while node != root && !links[node].red {
        let side = if links[p].children[0] == node { 0 } else { 1 };
        let other = 1 - side;
        var sibling = links[p].children[other];
        if links[sibling].red {
            links[sibling].red = false;
            links[p].red = true;
            rotate(links, root, p, side);
            sibling = links[p].children[other];
        }
        if !links[links[sibling].children[side]].red && !links[links[sibling].children[other]].red {
            links[sibling].red = true;
            node = p;
            p = links[node].parent;
        } else {
            if !links[links[sibling].children[other]].red {
                let near = links[sibling].children[side];
                links[near].red = false;
                links[sibling].red = true;
                rotate(links, root, sibling, other);
                sibling = links[p].children[other];
            }
            links[sibling].red = links[p].red;
            links[p].red = false;
            let far = links[sibling].children[other];
            links[far].red = false;
            rotate(links, root, p, side);
            node = root;
        }
    }
    links[node].red = false;
}

fn put<C, K, V>(tree: Tree<K, V>&, key: K, value: V) -> V, bool {
    var node = tree.root;
    var parent = 0;
    var side = 0;
    while node != 0 {
        parent = node;
        match C(key, tree.entries[node - 1].key) {
            Less => { side = 0; },
            Greater => { side = 1; },
            Equal => {
                let old = tree.entries[node - 1].value;
                tree.entries[node - 1].value = value;
                return old, true;
            }
        }
        node = tree.links[node].children[side];
    }
    let index = tree.links.len;
    tree.entries.push(Entry<K, V> { key: key, value: value });
    tree.links.push(Links { parent: parent, red: true });
    if parent == 0 { tree.root = index; }
    else { tree.links[parent].children[side] = index; }
    balance_insert(tree.links, tree.root, index);
    return value, false;
}

fn remove<C, K, V>(tree: Tree<K, V>&, key: K) {
    let target = find(tree, key, C);
    if target == 0 { return; }
    let removed = if tree.links[target].children[0] == 0 || tree.links[target].children[1] == 0 {
        target
    } else {
        next(tree, target)
    };
    let left = tree.links[removed].children[0];
    let replacement = if left != 0 { left } else { tree.links[removed].children[1] };
    let parent = tree.links[removed].parent;
    if replacement != 0 { tree.links[replacement].parent = parent; }
    attach(tree.links, tree.root, parent, removed, replacement);
    if !tree.links[removed].red {
        balance_delete(tree.links, tree.root, replacement, parent);
    }
    if removed != target {
        let inherited = tree.links[target];
        tree.links[removed] = inherited;
        for child in inherited.children {
            if child != 0 { tree.links[child].parent = removed; }
        }
        attach(tree.links, tree.root, inherited.parent, target, removed);
    }
}
