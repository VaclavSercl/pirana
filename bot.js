import 'dotenv/config';
import { Telegraf } from 'telegraf';
import { execFile } from 'child_process';
import { promisify } from 'util';
import { getBinaryPath, buildAgentCommand, parseOutput } from 'zeroclaw';
import path from 'path';
import { fileURLToPath } from 'url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const execFileAsync = promisify(execFile);

const bot = new Telegraf(process.env.TELEGRAM_BOT_TOKEN);

console.log('Spouštím ZeroClaw Telegram Bota...');

bot.start((ctx) => ctx.reply('Ahoj! Jsem ZeroClaw AI bot poháněný modelem Gemini. Jak mohu pomoci?'));

bot.on('text', async (ctx) => {
    // Validate Chat ID
    const allowedChatId = process.env.TELEGRAM_ALLOWED_CHAT_ID;
    if (allowedChatId && ctx.chat.id.toString() !== allowedChatId) {
        console.warn(`Neautorizovaný pokus o komunikaci z chatu ID: ${ctx.chat.id}`);
        return; // Můžeme tiše ignorovat
    }

    const userMessage = ctx.message.text;
    
    // Check if the user has provided the Gemini API Key, since we stored a placeholder
    const geminiKey = process.env.GEMINI_API_KEY;
    if (!geminiKey || geminiKey === 'YOUR_GEMINI_API_KEY_HERE') {
        return ctx.reply('⚠️ Nastala chyba proxy. Nejprve mi prosím nainstaluj nebo nastav GEMINI_API_KEY v souboru .env napojeném na tento server v D:\\Wendy\\AI\\Caslav\\zeroclaw_telegram_bot\\.');
    }

    try {
        await ctx.sendChatAction('typing');

        const binary = getBinaryPath("linux-x64"); // Binárka existuje pouze pro Linux a macOS. Spouštět to musíme přímo na Caslavi!
        
        // Získání správné cesty k aktuálnímu konfiguračnímu adresáři
        const zcConfigDir = __dirname;
        
        // Vytvoření command-line balíčku pro ZeroClaw engine
        const { cmd, args, env } = buildAgentCommand(binary, userMessage, {
            ZEROCLAW_CONFIG_DIR: zcConfigDir,
            GEMINI_API_KEY: geminiKey // Předáme klíč skrz prostředí
        });

        // Samotné volání enginu
        const result = await execFileAsync(cmd, args, { env });
        const output = parseOutput(result.stdout, result.stderr, result.exitCode);

        // Odeslání odpovědi
        if (output && output.message) {
            await ctx.reply(output.message);
        } else {
            await ctx.reply('Omlouvám se, ale AI neposkytla platnou odpověď.');
        }

    } catch (err) {
        console.error('Chyba komunikace s enginem ZeroClaw:', err);
        ctx.reply('❌ Při zpracování ZeroClaw agenta nastala kritická chyba! Otevři logy pro více informací.');
    }
});

// Zajištění gracefull shutdownu
bot.launch().then(() => {
    console.log('ZeroClaw Bot úspěšně aktivován a naslouchá!');
}).catch(e => {
    console.error('Chyba spuštění Telegram Bota. Pravděpodobně chybný token.', e);
});

process.once('SIGINT', () => bot.stop('SIGINT'));
process.once('SIGTERM', () => bot.stop('SIGTERM'));
