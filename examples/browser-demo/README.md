# browser-demo

## Lancer

```bash
cargo run -p browser-demo
```

## Tester

1. Ouvre http://localhost:3000 dans un premier onglet
2. Ouvre http://localhost:3000 dans un second onglet
3. Autorise micro + caméra dans les deux onglets
4. La connexion P2P s'établit automatiquement
5. Le flux vidéo/audio de chaque onglet apparaît dans l'autre

## Ce que fait ce demo

Le serveur Rust joue uniquement le rôle de **signaling relay** dans cet exemple.
Le media transite directement entre les deux onglets (P2P WebRTC natif du navigateur).
Pour que le media transite par Rust, utilise `seameet::Room::add_participant`.
