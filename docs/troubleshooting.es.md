# Solución de problemas

[English](troubleshooting.md) | Español

Casi todos los problemas vienen de la red o del firewall, así que revisa eso primero.

## El teléfono no encuentra la computadora

Primero asegúrate de que Litz esté corriendo en la computadora. En Linux:

```sh
systemctl --user status litzd
```

En Windows, busca `litzd.exe` en el Administrador de tareas. Arranca cuando inicias sesión. Si no aparece, abre una terminal y ejecuta `litzdw`.

Si está corriendo, revisa la red:

- El teléfono y la computadora tienen que estar en la misma red. Una red Wi-Fi de invitados no sirve, y tampoco un router con "aislamiento de clientes" o "aislamiento de AP" activado, porque los dos impiden que los dispositivos se vean entre sí.
- Una VPN en cualquiera de los dos puede esconder uno del otro. Apágala o permite el tráfico de red local en su configuración.
- El teléfono encuentra computadoras con mDNS (puerto UDP 5353). Si el firewall lo bloquea, la computadora nunca aparece en **Cerca**.

¿Sigue sin aparecer? Toca **Vincular por dirección IP** en la app y escribe la dirección local de la computadora, algo como `192.168.1.20`. Así te saltas la búsqueda.

## "Está en tu red, pero no responde"

El teléfono ve la computadora, pero el firewall está bloqueando el puerto de audio. Permite UDP 47165 en la computadora.

Ubuntu usa ufw. Primero ejecuta `sudo ufw status`. Si dice inactive, el problema no es el firewall.

```sh
sudo ufw allow 47165/udp
sudo ufw allow 5353/udp
```

Fedora trae firewalld activado:

```sh
sudo firewall-cmd --permanent --add-port=47165/udp --add-service=mdns
sudo firewall-cmd --reload
```

Arch Linux no instala firewall por defecto. Si configuraste ufw o firewalld, usa los comandos de arriba. Si usas el `/etc/nftables.conf` que viene por defecto, agrega esta línea a la cadena `input` y ejecuta `sudo systemctl restart nftables`:

```
udp dport { 5353, 47165 } accept
```

En NixOS, `services.litz-audio.enable = true` ya abre el puerto.

En Windows, ve a **Seguridad de Windows → Firewall y protección de red → Permitir una aplicación a través del firewall** y permite `litzd.exe` en redes privadas. Revisa también que tu Wi-Fi esté configurado como red privada y no pública.

## No aparece el aviso de vinculación en la computadora

El aviso es una notificación del escritorio con los botones **Allow** y **Deny** (por ahora solo en inglés). Si el teléfono dice que la computadora no pudo mostrarlo, desactiva el modo No molestar y revisa que las notificaciones de Litz Audio estén activadas.

Si usas un gestor de ventanas sin escritorio completo, necesitas un daemon de notificaciones que soporte botones de acción, como dunst o mako.

## "Esta computadora ya no confía en este teléfono"

Alguien quitó el teléfono desde la computadora con `litzd forget`. Vuelve a vincularlo desde la pantalla **Computadoras**.

## "La identidad de esta computadora cambió"

La computadora tiene una clave nueva. Eso pasa cuando se borraron los datos de Litz o se reinstaló el sistema. Si fuiste tú, vuelve a vincularla y listo. Si no, hay otra cosa en tu red respondiendo con el nombre de esa computadora, así que no la vincules.

## "Actualiza Litz en tu teléfono y en tu computadora"

La app y `litzd` están en versiones que no se entienden entre sí. Instala la última versión en los dos.

## Las apps de la computadora no oyen el teléfono

En la app con la que llamas o grabas, elige el micrófono correcto:

- Linux: **Litz Microphone**
- Windows: **CABLE Output**. Necesita [VB-CABLE](https://vb-audio.com/Cable/). Sin él, el teléfono te avisa que la computadora no tiene un micrófono virtual.

Revisa también que **Silenciar** esté desactivado en la app de Litz.

## Escuchar está activado pero el teléfono no suena

En la pantalla de inicio, abre **Sonido desde** y elige la salida que de verdad está sonando. **Salida predeterminada** sigue a la que esté usando la computadora.

## La computadora oye el altavoz del teléfono

Con el micrófono y Escuchar activados a la vez, el micrófono capta lo que suena en el teléfono. Con audífonos se arregla. Si no tienes, ve a **Ajustes → Modo de grabación** y elige **Llamadas y reuniones**, que usa la cancelación de eco del teléfono si la tiene.

## El audio se corta o suena entrecortado

Casi siempre es el Wi-Fi. Acércate al router, cámbiate a 5 GHz o conecta el teléfono por USB y activa el anclaje USB. Mientras estés por Wi-Fi, **Abrir Ajustes** en la tarjeta de la computadora en Litz te lleva a ese interruptor. Para que se active solo cada vez que conectes el cable, entra en las **Opciones para desarrolladores** del teléfono y elige el anclaje USB como configuración USB predeterminada. Mientras pruebas, **Ajustes → Acerca de → Diagnóstico** muestra los paquetes perdidos y los que llegan tarde.

## Solo un teléfono a la vez

Una computadora transmite con un solo teléfono a la vez. Si activas el micrófono o Escuchar en un segundo teléfono vinculado, el audio se pasa a ese.

## Si nada de esto funcionó

Abre un [issue](https://github.com/ArturoAtomplay/litz-audio/issues/new/choose) (en español está bien) y pega el reporte de **Ajustes → Acerca de → Diagnóstico**. Agrega también el log de la computadora. En Linux:

```sh
journalctl --user -u litzd -e
```

Agrega `-p warning` para ver solo los problemas. En Windows el log está en `%LOCALAPPDATA%\litz\litzd.log`. Para más detalle, inicia litzd con `LITZ_LOG=debug`.
