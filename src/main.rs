#![allow(proc_macro_derive_resolution_fallback)]
extern crate time;
extern crate chrono;
extern crate r2d2;
extern crate uuid;
#[macro_use]
extern crate diesel;
#[macro_use]
extern crate diesel_migrations;
extern crate serde;
extern crate serde_json;
#[macro_use]
extern crate serde_derive;
extern crate kankyo;
extern crate envy;
extern crate typemap;
extern crate discord_bots;
#[macro_use]
extern crate serenity;

extern crate iron;
extern crate handlebars_iron;
extern crate staticfile;
extern crate mount;
extern crate router;
extern crate params;
extern crate oauth2;
extern crate reqwest;
extern crate bincode;
extern crate iron_sessionstorage_0_6;

pub mod schema;
pub mod data;
#[macro_use]
pub mod model;
pub mod web;
pub mod bottle;

use std::thread;
use std::fs::File;
use std::sync::{Arc};
use time::Duration;
use diesel::pg::PgConnection;
use diesel::r2d2::ConnectionManager;

use serenity::prelude::*;
use serenity::framework::standard::{Args, CommandError, DispatchError, StandardFramework, HelpBehaviour, CommandOptions, help_commands};
use serenity::model::channel::{Message, Channel, Embed, Attachment, Reaction};
use serenity::model::event::*;
use serenity::model::gateway;
use serenity::model::permissions::Permissions;
use serenity::CACHE as cache;

use data::*;
use model::*;
use model::id::*;

const ADMIN_PERM: Permissions = Permissions::ADMINISTRATOR;

fn update_guilds(ctx: &Context) {
    let stats = discord_bots::PostBotStats::new(discord_bots::ServerCount::Single(cache.read().all_guilds().len()));
    ctx.get_bots().post_stats(stats).unwrap();
}

struct Handler;
impl EventHandler for Handler {
    fn message(&self, ctx: Context, new_message: Message) {
        if !new_message.author.bot {
            let conn = ctx.get_conn();

            let res = match new_message.channel() {
                Some(Channel::Guild(ref channel)) => {
                    let channel = channel.read();
                    let gid = channel.guild_id.as_i64();
                    let guilddata = Guild::get(gid, &conn);

                    if Some(channel.id.as_i64()) == guilddata.bottle_channel {
                        bottle::new_bottle(&new_message, Some(gid), ctx.get_pool(), ctx.get_cfg())
                    } else {
                        Ok(None)
                    }
                },

                Some(Channel::Private(_)) => bottle::new_bottle(&new_message, None, ctx.get_pool(), ctx.get_cfg()),
                _ => Ok(None)
            };

            match res {
                Ok(Some(x)) => new_message.reply(&x).ok(),
                Err(x) => new_message.reply(x.description()).ok(),
                _ => None
            };
        }
    }

    fn reaction_add(&self, ctx: Context, r: Reaction) {
        let conn = &ctx.get_conn();
        bottle::react(conn, r, true, &ctx.get_cfg()).unwrap();
    }

    fn reaction_remove(&self, ctx: Context, r: Reaction) {
        let conn = &ctx.get_conn();
        bottle::react(conn, r, false, &ctx.get_cfg()).unwrap();
    }

    fn guild_create (&self, ctx: Context, guild: serenity::model::guild::Guild, is_new: bool) {
        let conn = ctx.get_conn();
        let guilddata = Guild::get(guild.id.as_i64(), &conn);
        let user = &cache.read().user;

        if is_new {
            let general = guild.channels.iter()
                .find(|&(channelid, _)| guild.permissions_in(channelid, user).send_messages());

            if let Some((channel, _)) = general {
                channel.send_message(|x|
                    x.content("Hey! If you want to receive and send bottles, please set the channel you want to receive them in with ``-configure``. Thanks!")).ok();
            }
        }

        guilddata.update(&conn).unwrap();
        update_guilds(&ctx);
    }

    fn guild_delete (&self, ctx: Context, incomplete: serenity::model::guild::PartialGuild, _full: Option<Arc<RwLock<serenity::model::guild::Guild>>>) {
        Guild::del(incomplete.id.as_i64(), &ctx.get_conn()).unwrap();

        update_guilds(&ctx);
    }

    fn ready(&self, ctx:Context, _data_about_bot: serenity::model::gateway::Ready) {
        ctx.set_presence(Some(gateway::Game {kind: gateway::GameType::Listening, name: "you, try -help".to_owned(), url: None})
                         , serenity::model::user::OnlineStatus::DoNotDisturb);

        let conn = &ctx.get_conn();
        let mut u = User::get(ctx.get_cfg().auto_admin, conn);
        u.admin = true;
        u.update(conn).ok();

        println!("Ready!");
    }
}

embed_migrations!("migrations/");

fn main() {
    kankyo::load_from_reader(&mut File::open("./.env").unwrap()).unwrap();
    let config:Config = envy::from_env::<Config>().unwrap();
    let manager = ConnectionManager::<PgConnection>::new(config.clone().database_url);
    let db = r2d2::Pool::builder().build(manager).expect("Error initializing connection pool.");

    embedded_migrations::run_with_output(&db.get_conn(), &mut std::io::stdout()).unwrap();

    let webdb = db.clone(); let webcfg = config.clone();
    thread::spawn( move || web::start_serv(webdb, webcfg));

    let dbots = Arc::new(discord_bots::Client::new(&config.discord_bots_token));

    let mut client = Client::new(&config.token, Handler).expect("Error initializing client.");
    client.data.lock().insert::<DBots>(dbots);
    client.data.lock().insert::<DConn>(db.clone());
    client.data.lock().insert::<DConfig>(config.clone());

    client.with_framework(StandardFramework::new()
        .configure(|c| c.prefix("-").dynamic_prefix(|ctx, msg| {
            let conn = &ctx.get_conn();

            msg.guild_id.and_then(|gid| Guild::get(gid.as_i64(), conn).prefix)
        }))
        .help(|_f, msg, _opts, _cmds, _args | {
              msg.reply ("Set a bottle channel with ``-configure <channel>``, then start sending out and replying (prefix your message with ``->`` to bottles there! Or dm me for anonymous bottles! :^) Also try ``-info``")?;

              Ok(())
        })
        .command("configure", |c|
            c.required_permissions(ADMIN_PERM)
                .guild_only(true)
                .exec(| ctx, msg, mut args: serenity::framework::standard::Args | {
                    let conn = &ctx.get_conn();
                    let mut guild = Guild::get(msg.guild_id.unwrap().as_i64(), &conn);

                    if let Ok(chan) = args.find::<serenity::model::channel::Channel>() {
                        guild.bottle_channel = Some(chan.id().as_i64());
                        guild.update(conn)?;

                        msg.reply("All set!")?;
                        Ok(())
                    } else if let Ok(x) = args.find::<char>() {
                        guild.prefix = Some(x.to_string());
                        guild.update(conn)?;

                        msg.reply(&format!("Set prefix to \"{}\"!", x))?;

                        Ok(())
                    } else {
                        Err("Please specify a valid channel or a single character prefix!".into())
                    }
                })
        )
        .command("mote", |c|
            c.exec(|ctx, msg, mut args| {
                let usr = args.single::<serenity::model::user::User>()
                    .map_err(|_| "Please specify a user to promote.")?;

                if ctx.get_cfg().auto_admin == msg.author.id.as_i64() {
                    let conn = &ctx.get_conn();
                    let mut u = User::get(usr.id.as_i64(), conn);
                    if !u.admin {
                        u.admin = true;
                        msg.reply(&format!("Promoted {}", usr.tag()))?;
                    } else {
                        u.admin = false;
                        msg.reply(&format!("Demoted {}", usr.tag()))?;
                    }

                    u.update(conn)?;
                    Ok(())
                } else {
                    Err("You must be an auto admin to do this!".into())
                }
            })
        )
        .command("info", |c|
            c.guild_only(true).exec(|ctx, msg, _args| {
                let conn = &ctx.get_conn();
                let gdata = Guild::get(msg.guild_id.unwrap().as_i64(), conn);

                let guild_channel = msg.channel().unwrap().guild().unwrap();
                let guild = guild_channel.read().guild().unwrap();

                guild_channel.read().send_message(|msg| msg.embed(|embed| {
                    let public = match gdata.invite.as_ref() {
                        Some(inv) => inv,
                        None => "Use -publicize to generate an invite!"
                    };

                    let bottle_channel = match gdata.bottle_channel.as_ref() {
                        Some(cid) => serenity::model::id::ChannelId(*cid as u64).mention(),
                        None => "Set with -configure <channel>".to_owned()
                    };

                    embed.title(guild.read().name.clone())
                        .field("XP", gdata.get_xp(conn).unwrap_or(None).unwrap_or(0), true)
                        .field("Bottle channel", bottle_channel, true)
                        .field("Public", public, true)
                        .url(guild_url(gdata.id, &ctx.get_cfg()))
                }))?;

                Ok(())
            })
        )
        .command("publicize", |c|
            c.guild_only(true).required_permissions(ADMIN_PERM)
                .exec(|ctx, msg, _args| {
                let conn = &ctx.get_conn();
                let mut gdata = Guild::get(msg.guild_id.unwrap().as_i64(), conn);

                let guildc = msg.channel().unwrap().guild().unwrap();
                let inv = guildc.read().create_invite(|x| x.max_age(0).temporary(true))?;
                gdata.invite = Some(inv.url());
                gdata.update(conn)?;

                msg.reply("Guild publicized!")?;
                Ok(())
            })
        )

        .on_dispatch_error(| _ctx, msg, err | {
            match err {
                DispatchError::LackOfPermissions(_) => {
                    msg.reply("You lack permission to do this! Please make sure you are an administrator.").ok();
                },
                _ => ()
            }
        })
        .after(|_ctx, msg, _, res| {
            if let Err(CommandError(str)) = res {
                msg.reply(&str).ok();
            }
        })
    );

    client.start_autosharded().unwrap();
}