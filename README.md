# wuget

Télécharge et déchiffre du contenu Wii U en une seule commande. Réunit ce que
faisaient FunKiiU (protocole CDN, tickets) et cdecrypt (déchiffrement AES,
extraction FST) dans un seul binaire Rust, sans Python ni outil externe.

```
Title ID ──▶ ticket ──▶ CDN Nintendo ──▶ déchiffrement ──▶ code/ content/ meta/
```

## Utilisation

```sh
wuget                                # sélecteur interactif (3621 titres)
wuget get 0005000010143500           # direct, par Title ID
wuget search zelda --region EUR      # cherche dans le catalogue
wuget decrypt <dump>                 # déchiffre un dump NUS existant
wuget ticket <id> [--generated]      # écrit un ticket sur la sortie standard
```

Options globales : `-o/--output` (défaut `~/Documents/Cemu/games`), `--keep`
(conserve les `.app`/`.h3`), `--no-decrypt`, `--jobs N` (téléchargements
simultanés, défaut 3), `--retry N`, `--no-patch-dlc`, `--no-patch-demo`.

Dans le sélecteur : taper filtre, `Tab` change de région, `⇧Tab` de type,
`Espace` coche plusieurs titres (quand la recherche est vide), `Entrée` lance.

## Tickets

Trois sources, par ordre de préférence :

1. **cetk Nintendo** pour les updates — légitime ;
2. **ticket légitime embarqué** (964 titres) — s'installe sans patch de
   signature sur console ;
3. **ticket généré** depuis la clé du catalogue — fonctionne dans Cemu, mais
   demande des patchs de signature sur du matériel réel.

La source retenue est affichée à chaque téléchargement.

## Sortie

`<output>/<Nom> [RÉGION]/{code,content,meta}`, directement chargeable dans Cemu
via *File ▸ Load* sur le `.rpx` de `code/`. Les fichiers chiffrés
intermédiaires sont supprimés après un déchiffrement réussi (`--keep` pour les
garder) ; en cas d'échec ils sont toujours conservés, pour ne pas avoir à
retélécharger.

## Vérification

Le portage est validé contre les outils d'origine :

- le ticket produit par `wuget ticket <id> --generated` est identique octet à
  octet à celui de FunKiiU ;
- la sortie de `wuget decrypt` est identique à celle de cdecrypt (`diff -r`
  silencieux sur 1018 fichiers / 1,7 Go de *The Wind Waker HD*) ;
- chaque bloc de contenu haché voit son SHA-1 H0 vérifié pendant l'extraction,
  donc une clé fausse échoue bruyamment plutôt que d'écrire des données
  corrompues.

`cargo test` couvre le parsing du catalogue, les deux chemins de ticket, la FST
et le sélecteur.

## Données embarquées

`data/` contient la base de clés (3621 titres), les 964 tickets légitimes, le
certificat commun, le gabarit de ticket et le patch de déverrouillage DLC.
Tout est compilé dans le binaire par `build.rs`, qui empaquette les tickets en
un blob unique indexé.

`reference/` conserve les sources C de cdecrypt ayant servi au portage.

## Licence et attribution

GPL-3.0-or-later, hérité de cdecrypt dont `src/decrypt.rs` et `src/fst.rs` sont
un portage.

- **cdecrypt** — © 2020-2023 VitaSmith, © 2013-2015 crediar, GPL-3.0.
  <https://github.com/VitaSmith/cdecrypt>
- **FunKiiU** — cearp et the cerea1killer ; `src/ticket.rs` et `src/download.rs`
  portent son protocole CDN et sa fabrication de ticket.

Le contenu de `data/` (base de clés, tickets) provient d'un miroir public de la
Wii U Title Key Database et n'est pas couvert par cette licence.
